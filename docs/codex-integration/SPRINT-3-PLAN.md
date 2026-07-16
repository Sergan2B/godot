# Sprint 3 plan — ResourceUID and resource dependency graph

**Status:** In progress — `S3-01`–`S3-05` complete locally; `S3-06` next

**Planned duration:** 10 working days

**Milestone:** Static Index Foundation

**Delivery risk:** High — persistent storage choice, Godot editor internals, migrations, and cross-platform recovery share one critical path

**Baseline:** `7845c3ffab` (`codex/integration`), Sprint 2 live-verified on Windows x86_64 and macOS arm64

**Parent scope:** [MASTER_SPRINT_ROADMAP.md](MASTER_SPRINT_ROADMAP.md), Sprint 3

**Stage 1 plan:** [SPRINT-3-STAGE-1-PLAN.md](SPRINT-3-STAGE-1-PLAN.md) — `S3-01`/`S3-02`
contracts, identity rules, and golden resource graph

**Stage 3 completion:** [SPRINT-3-STAGE-3-PLAN.md](SPRINT-3-STAGE-3-PLAN.md) —
`S3-04`/`S3-05` Bridge RPC 1.2, ResourceGraphAdapter, journal, and Rust wire-client

## 1. Outcome

Sprint 3 delivers the first persistent, project-scoped static index. Godot remains
authoritative for resource identity, editor-known file/import state, and declared
resource dependencies. The Rust sidecar normalizes that data, commits atomic index
generations, maintains reverse edges, and exposes two bounded read-only MCP tools:

- `godot_get_resource_dependencies`;
- `godot_find_resource_owners`.

The sprint is complete only when a resource rename preserves UID identity and updates
reverse usages incrementally, a stale UID is diagnosed explicitly, a compatible cache
is reused after reopen, and cancelled/corrupt rebuilds cannot replace the last valid
generation.

This closes the resource-only foundation slice of `R1-02`/`R1-03`; full find-usages
and scene explanation remain open through Sprints 4–6.

## 2. Starting point

Sprint 2 provides:

- Bridge RPC 1.1 negotiation, authenticated local transport, revision/event semantics,
  and checksum-verified chunked snapshots;
- `EditorContextAdapter` for live scene, selection, Inspector values, and dirty state;
- a Rust `SnapshotReplicator` with atomic in-memory activation and stale-state denial;
- three project-scoped read-only MCP tools;
- live editor → bridge → sidecar → MCP gates on Windows and macOS.

Sprint 3 starts without:

- a `ResourceGraphAdapter` or resource filesystem change queue;
- persistent `IndexStore`, index migrations, or crash recovery;
- a canonical resource/dependency DTO in the schema bundle;
- resource dependency or reverse-owner MCP tools.

Stage 1 now provides the frozen resource contract, canonical identity vectors, and an
independent 8-phase resource golden fixture/truth graph. See
[SPRINT-3-STAGE-1-PLAN.md](SPRINT-3-STAGE-1-PLAN.md) for local evidence. The remaining
items above begin with the `S3-03` storage spike and `S3-04` Bridge RPC schema work.

## 3. Scope boundaries

### 3.1 In scope

- `ResourceUID`, normalized `res://` paths, Godot resource type, import state,
  modification metadata, bounded content hash, and validity;
- declared direct dependencies from `ResourceLoader`/`EditorFileSystem` and derived
  reverse edges;
- `.tscn`, `.scn`, `.tres`, `.res`, and imported resources;
- full catalog build, incremental upsert/move/remove/reimport, invalidation, rebuild,
  cancellation, corruption isolation, compatible reopen, and schema migration;
- Bridge RPC schemas/capabilities needed for resource snapshot and delta transfer;
- persistent static generation in the sidecar and the two Sprint 3 MCP tools;
- deterministic golden, cross-language, fault, performance, and live tests.

### 3.2 Explicitly out of scope

- `PackedScene` node/inheritance/instance semantics, which belong to Sprint 4;
- GDScript/C# symbol extraction and dynamic `load(variable)` resolution;
- the full evidence aggregation vocabulary planned for Sprint 6;
- dirty editor overlay merge, which remains Sprint 7 scope;
- runtime observations and write/Undo transactions;
- persistent node/subresource identity decision `D-06`;
- arbitrary graph-query language or storage-specific MCP parameters.

The index may store scene/script files as Resource entities and dependency endpoints,
but it must not claim node, symbol, or runtime semantics during Sprint 3.

## 4. Required contracts and invariants

### 4.1 Source authority

| Fact | Authoritative source | Rule |
|---|---|---|
| UID ↔ current path | `ResourceUID` | UID wins identity; path is mutable metadata |
| File/type/import state | `EditorFileSystem` | Sidecar does not infer editor import state from extensions |
| Declared dependencies | `ResourceLoader::get_dependencies` and editor filesystem data | Exact only for Godot-declared edges |
| Content hash | Bounded raw-file stream on the canonical project root | Hash proves bytes, not Godot semantics |
| Reverse dependency | Derived from committed direct edges | Must share the same index revision |

Raw `.tscn` parsing in the sidecar is not a substitute for Godot dependency APIs.
Absolute local paths, token material, and imported payload contents are not persisted in
semantic result objects or ordinary logs.

The sidecar hashes source bytes on a bounded worker after converting the editor-provided
`res://` path through a project-root path policy that rejects traversal, symlink, and
reparse-point escape. It verifies size/mtime before and after the stream; a concurrent
change discards the hash and requeues the resource instead of committing mixed metadata.

### 4.2 Resource identity

`INDEX-001` must freeze these rules before adapter/index implementation merges:

1. A valid UID produces one deterministic resource entity ID independent of path.
2. A resource without a UID uses normalized `res://` path plus content generation as a
   weaker identity and is marked accordingly.
3. Rename with the same UID updates `current_path` without replacing the entity ID.
4. A dependency UID that no longer resolves produces `stale_resource_uid`; it is not
   silently rebound to an unrelated path.
5. Deletion creates a bounded tombstone/diagnostic for inbound references until the
   committing generation has reconciled every affected edge.
6. Entity IDs and cursors are opaque to MCP clients.

### 4.3 Atomic index generation

- Full build writes a staging generation and activates it in one durable commit or
  atomic pointer swap.
- Incremental batches require the exact previous resource revision/checkpoint.
- Resource upserts/removals, direct edges, reverse edges, diagnostics, and checkpoint
  advance commit atomically.
- `index_revision` advances only after the durable commit succeeds.
- Queries never observe half-applied generations.
- Cancellation or crash leaves the previous valid generation queryable.
- An incompatible or corrupt cache is quarantined and rebuilt; it is never presented
  as current.
- Reconnect does not force a rebuild when project identity, index schema, and source
  validation checkpoint are compatible.

### 4.4 Threading and limits

- `ResourceUID`, `EditorFileSystem`, and editor singletons are accessed only on the
  Godot main thread through `ResourceGraphAdapter`.
- Initial catalog traversal and filesystem diffing are resumable and stay inside the
  existing 2 ms main-thread budget.
- DTOs are detached before hashing, serialization, IPC, or persistent writes.
- Bulk indexing has lower priority than control/status/selection traffic.
- Queues, resource count, dependency count, path length, per-batch bytes, query result
  size, and cursor lifetime have explicit hard limits.
- Queue/journal overflow emits structured invalidation and triggers a full resource
  resnapshot; it never drops structural changes silently.

## 5. Planned Bridge RPC 1.2 extension

Sprint 3 introduces a backward-compatible Bridge RPC 1.2 profile. Version 1.1 remains
available for Sprint 2 clients. The exact JSON Schema is frozen before C++ and Rust
implementations are accepted.

A 1.2 sidecar may fall back to a 1.1 bridge for the three Sprint 2 tools, but resource
tools remain unavailable with a structured capability error; the sidecar must not
emulate Godot resource semantics locally.

### 5.1 Capabilities

- `resource.uid_dependencies` — ResourceUID, file/import metadata, and declared edges;
- `resource.incremental_index` — ordered resource revisions/deltas and gap recovery;
- existing `sync.full_snapshot_v1` and `sync.event_stream_v1` remain unchanged for
  editor context.

### 5.2 Messages

| Message | Purpose |
|---|---|
| `resource.snapshot.get` | Request a full resource catalog at a consistent resource revision |
| `snapshot.begin/chunk/end` with resource domain | Transfer bounded Resource, edge, and diagnostic DTOs using existing checksum/ack rules |
| `sync.event` / `resource_graph_changed` | Announce that a newer resource revision is available |
| `resource.delta.get` | Fetch ordered upsert/move/remove/reimport operations after an exact revision |
| `sync.invalidated` / `resource_journal_gap` | Force full resource snapshot after gap, overflow, or incompatible checkpoint |

The resource delta journal is independent from persistent storage: the bridge owns only
bounded editor-session change history, while the sidecar owns durable static generations.
Move detection uses stable UID when available; otherwise it is represented as remove plus
upsert and must not claim persistent identity.

### 5.3 DTO minimum

The Bridge DTO contains editor facts only: UID or explicit `uid_missing`, normalized
project-relative path, Godot type, import/source state, modification time, byte size,
validity, declared dependency references, resolution status, diagnostics, authority,
and resource revision. Opaque entity/edge IDs, content hashes, content generations,
and UID-less identity are calculated by the sidecar during `S3-06`/`S3-07`; the bridge
does not preempt that ownership boundary.

The schema fixtures include valid full snapshot, valid delta, rename, deletion,
reimport, stale UID, invalid revision, oversized batch, corrupt checksum, and journal
gap cases.

## 6. `INDEX-001` and storage decision `D-05`

### 6.1 Logical model implemented in Sprint 3

The storage abstraction must represent at least:

- index metadata: schema version, project ID, active generation, build state, hash
  algorithm, and compatibility range;
- resource identities: entity ID, UID, current path, identity strength, type, import
  state, content generation, and validity;
- source documents: mtime, size, content hash, and parse/ingest state;
- direct dependency relations and a reverse lookup index;
- diagnostics for stale UID, missing dependency, invalid import, hash failure, and
  incompatible cache;
- ingestion checkpoint: editor session, event/resource revision, project revision, and
  durable index revision;
- staging generation metadata needed for cancellation and recovery.

The public Rust interfaces are storage-neutral: `IndexRead`, `IndexWriteTransaction`,
`GenerationBuilder`, `MigrationRunner`, and `ResourceQuery`. No SQL, file layout, or
backend cursor leaks into Bridge RPC or MCP.

### 6.2 Mandatory spike

During the first half of the sprint, `IDX-001A` compares:

- SQLite at `.godot/codex/index.sqlite`;
- a directory/segment store at `.godot/codex/index/`.

Both implementations receive the same normalized input and fault schedule. The report
records:

- full build and incremental rename time;
- reverse-edge cached query p50/p95;
- concurrent read behavior during staging/activation;
- write amplification and artifact size;
- cancellation at capture, staging, pre-commit, and post-commit boundaries;
- crash recovery and corrupt-cache isolation;
- v1 → v2 migration behavior;
- macOS, Windows, and Linux process-level locking/reopen behavior;
- dependency and single-binary packaging impact.

Correctness/recovery failures disqualify a backend regardless of speed. Decision `D-05`
and rejected alternatives are recorded in `INDEX-001` by the end of working day 5.

## 7. MCP contract

Both tools are read-only, idempotent, project-scoped, bounded, and backed only by a
committed compatible static generation.

### 7.1 Common input

```text
resource: uid://... | res://...
limit: 100 by default, bounded to 1..200
cursor: optional opaque cursor bound to project, generation, query, and expiry
```

Malformed paths, absolute paths, traversal, unknown fields, invalid limit, and a
cursor from another project/generation are rejected with structured errors.

### 7.2 `godot_get_resource_dependencies`

Returns direct outgoing dependencies. Results are deterministic and paginated and
include resolution status for each edge. Recursive traversal is intentionally deferred
until the later query/evidence core.

### 7.3 `godot_find_resource_owners`

Returns resources that directly depend on the queried resource. It uses committed
reverse edges and the same ordering and pagination rules as the forward query.

### 7.4 Common result

Every success includes:

- project ID, index schema version, generation, index revision, and validated resource
  checkpoint;
- queried resource identity and identity strength;
- deterministic resource/edge records;
- status, freshness, truncation, diagnostics, and next cursor;
- evidence source (`resource_uid_registry`, `editor_file_system`, or
  `resource_loader_dependencies`) and revision coordinates.

A known-but-unresolved UID returns a partial result with `stale_resource_uid`. A value
that has never been indexed returns `resource_not_found`. Before cache validation or
after a revision gap, tools return `index_not_current` rather than labeling stale data
as current.

## 8. Work packages

| ID | Priority | Work | Output | Exit evidence |
|---|---:|---|---|---|
| `S3-01` | P0 | Freeze resource identity, logical schema, generation, migration, and query rules | `INDEX-001` draft; [Stage 1 plan](SPRINT-3-STAGE-1-PLAN.md) | Reviewed contract and canonical examples |
| `S3-02` | P0 | Build independent resource golden fixture/truth graph | Fixture, generator, golden JSON; [Stage 1 plan](SPRINT-3-STAGE-1-PLAN.md) | Truth validation independent of index implementation |
| `S3-03` | P0 | Run SQLite vs segment-store spike | Raw benchmark/fault JSON and `D-05` decision | Correctness matrix and chosen backend |
| `S3-04` | P0 | Extend Bridge RPC schemas and conformance to 1.2 | Schemas, fixtures, negotiation tests | C++/Rust accept valid and reject invalid cases |
| `S3-05` | P0 | Implement `ResourceGraphAdapter` and bounded change queue | C++ adapter/service integration | Main-thread budget and catalog/delta tests |
| `S3-06` | P0 | Implement storage-neutral resource normalizer and chosen `IndexStore` | Rust modules and migrations | Atomic generation and reverse-edge tests |
| `S3-07` | P0 | Implement incremental ingestion, rebuild, cancellation, and recovery | Sidecar coordinator | Rename, delete, reimport, gap, crash, and reopen tests |
| `S3-08` | P0 | Add the two MCP tools with pagination/errors | MCP contract and tests | Golden forward/reverse queries without a model |
| `S3-09` | P0 | Add cross-platform live resource-index smoke | Harness and normalized evidence | Windows/macOS editor → MCP gate |
| `S3-10` | P0 | Final audit, docs, metrics, and evidence | `SPRINT-3-EVIDENCE.md` | Requirement-by-requirement completion report |

`S3-01`/`S3-02` may proceed together. The storage-neutral interface can be written
before `D-05`, but only the chosen backend remains production code. `S3-08` does not
merge before generation/freshness semantics and contract tests are fixed.

Local progress through 2026-07-16: `S3-01` and `S3-02` are implemented and verified against
11 Python contract tests, 2 focused Draft 2020-12 Rust tests, the full 10-test Rust
conformance suite, and all 8 live Godot fixture phases. `S3-03` is complete for the
local implementation stream: the storage-neutral API, both spike backends, and strict
schema-v2 benchmark/fault harness are implemented, and the full macOS `10k/50k` plus
`100k/500k` run passes every candidate gate and selects the segment store for `D-05`.
Windows/Linux portability verification is `not_run` and remains separate from remote
CI. `S3-04` and `S3-05` are also complete locally: Bridge RPC 1.2,
`ResourceGraphAdapter`, the bounded incremental journal, and the production Rust
wire-client pass the eight-phase live gate plus Sprint 1/2 regressions. `S3-06`–`S3-10`
remain open.

### 8.1 Parent-roadmap traceability

| Parent Sprint 3 work | Work packages | Acceptance |
|---|---|---|
| Define semantic-index schema | `S3-01`, `S3-03` | `S3-AC-04`–`S3-AC-06` |
| Index UID/path/type/import/mtime/hash | `S3-05`, `S3-06` | `S3-AC-03`, `S3-AC-07` |
| Read dependencies through Godot | `S3-04`, `S3-05` | `S3-AC-01` |
| Build reverse dependency edges | `S3-06`, `S3-08` | `S3-AC-01`, `S3-AC-09` |
| Cover text/binary/imported resources | `S3-02`, `S3-09` | `S3-AC-07` |
| Handle move/rename/delete/reimport | `S3-05`, `S3-07` | `S3-AC-02`, `S3-AC-08` |
| Add incremental update queue | `S3-05`, `S3-07` | `S3-AC-02`, `S3-AC-11` |
| Move heavy persistence off main thread | `S3-06`, `S3-07` | `S3-AC-10`, `S3-AC-11` |
| Add invalidation and full rebuild | `S3-04`, `S3-07` | `S3-AC-05`, `S3-AC-06`, `S3-AC-08` |
| Add index format migration/version | `S3-01`, `S3-03`, `S3-06` | `S3-AC-04`, `S3-AC-06` |

The required artifacts map to `INDEX-001` (`S3-01`/`S3-03`), the indexer
(`S3-05`–`S3-07`), two MCP tools (`S3-08`), and golden dependency fixtures
(`S3-02`).

## 9. Ten-day execution sequence

| Day | Focus | Required exit |
|---:|---|---|
| 1 | Freeze scope, identity vocabulary, fixture truth format, and protocol sketch | `INDEX-001` skeleton and reviewed fixture map |
| 2 | Build text/binary/imported fixture generation and canonical truth graph | Golden baseline validates independently |
| 3 | Implement storage-neutral interfaces and both spike backends | Identical ingest/query oracle runs |
| 4 | Run performance, cancellation, corruption, and locking fault matrix | Raw results archived; correctness gaps fixed or backend rejected |
| 5 | Freeze `D-05`, schema v1, migration policy, and Bridge RPC 1.2 | `INDEX-001` decision and schema fixtures pass |
| 6 | Implement C++ resource catalog/delta adapter and main-thread slicing | C++ unit/live adapter tests pass |
| 7 | Implement production store, normalization, full build, and activation | Atomic full generation and reopen pass |
| 8 | Implement incremental move/remove/reimport, gap recovery, and migration | No-rebuild rename plus fault tests pass |
| 9 | Implement MCP tools, pagination, structured errors, and model-free E2E | Golden forward/reverse results match exactly |
| 10 | Windows/macOS live gates, performance SLO audit, docs, and completion review | Evidence package complete; no required gate open |

If `D-05` is not frozen by day 5, feature implementation stops at the storage-neutral
boundary. The sprint is not declared complete by shipping an unmeasured provisional
backend.

The day numbers express gate order for one execution stream, not permission to merge
dependent work early. Additional contributors may parallelize fixture, schema, and
backend-spike work only against the same frozen contracts. If the critical path exceeds
10 days, the parent scope and acceptance remain open rather than being silently cut.

## 10. Fixture and golden truth design

Create `tests/codex/fixtures/resource_graph_project` with deterministic setup, paired
with an implementation-independent oracle in
`tests/codex/fixtures/resource_graph_oracle`. The detailed file map, identity vectors,
mutation phases, and stage gate are fixed in
[SPRINT-3-STAGE-1-PLAN.md](SPRINT-3-STAGE-1-PLAN.md).

The fixture contains:

- `.tres` and `.res` resources with a shared dependency;
- `.tscn` and `.scn` scenes represented only as resources during Sprint 3;
- at least one imported source asset with valid import metadata;
- a direct chain, fan-in, fan-out, cycle, orphan, missing target, and stale UID;
- two distinct resources with equal content to prove identity is not content equality;
- a path containing spaces and Unicode;
- deterministic mutation phases: rename, delete, re-add, reimport, and content edit;
- a generated medium graph for performance and cancellation, separate from semantic
  truth fixtures.

Binary `.res`/`.scn` files are generated reproducibly by the enabled Godot fixture setup
and verified against semantic truth; the golden oracle never parses the production
index backend.

## 11. Acceptance matrix

| ID | Requirement | Proof |
|---|---|---|
| `S3-AC-01` | Direct and reverse dependencies exactly match the golden graph | Canonical graph diff, no missing/extra edge |
| `S3-AC-02` | UID-preserving rename updates path and reverse owners without full rebuild | Same entity ID; `full_rebuild_count` unchanged; one incremental commit |
| `S3-AC-03` | Stale UID is explicit and never rebound silently | Structured `stale_resource_uid` diagnostic and negative fixture |
| `S3-AC-04` | Compatible cache is reused after editor/sidecar reopen | Same generation/index revision after validation; no rebuild |
| `S3-AC-05` | A cancelled rebuild cannot replace current generation | Fault injection at every cancellation boundary |
| `S3-AC-06` | Corrupt/incompatible cache is quarantined and rebuilt safely | Previous valid data never presented as current; project files unchanged |
| `S3-AC-07` | `.tscn`, `.scn`, `.tres`, `.res`, and imported resources are indexed | Format/import matrix in golden evidence |
| `S3-AC-08` | Delete, re-add, reimport, and event gap reconcile deterministically | Delta trace plus full-resnapshot fallback |
| `S3-AC-09` | Forward/reverse MCP tools are bounded, paginated, and project-scoped | Schema negatives, cursor isolation, ordering, and limit tests |
| `S3-AC-10` | Cached query and change visibility satisfy initial SLOs | Raw p50/p95 report on reference fixtures |
| `S3-AC-11` | Bulk indexing does not starve control/live context | Ping/status p95 and main-thread telemetry during rebuild |
| `S3-AC-12` | Windows and macOS production chains return the same normalized graph | Cross-platform model-free live evidence |

## 12. Performance gates

Use the initial product SLOs until Sprint 3 measurements justify a documented update:

- cached dependency/owner query p95 ≤ 300 ms;
- changed-file visibility in committed index p95 ≤ 2 s;
- status/ping during bulk indexing p95 ≤ 200 ms;
- unhandled editor stalls beyond the 2 ms bridge frame budget: zero on reference
  fixtures.

Evidence includes fixture size, resource/edge counts, warm/cold state, iterations,
host/OS, storage backend/version, artifact size, and raw samples. Averages alone are
insufficient.

## 13. Test and evidence layers

### 13.1 Schema and contract

- Draft 2020-12 validation for every Bridge RPC 1.2 schema/fixture;
- protocol negotiation 1.2 ↔ 1.2 and compatibility fallback 1.2 client ↔ 1.1 bridge;
- duplicate JSON members, invalid context/revision, bad checksum, oversized batch,
  traversal path, stale cursor, and unknown-field negatives.

### 13.2 Godot C++

- UID/path/type/import/dependency projection;
- bounded traversal and detached DTO ownership;
- change coalescing, UID rename detection, delete/reimport, queue overflow, and gap;
- main-thread budget telemetry and shutdown/cancellation;
- editor-only build guard and focused/full Godot suites.

### 13.3 Rust sidecar

- normalizer and deterministic entity/edge ordering;
- property-based cycle/fan-in/fan-out and forward/reverse parity;
- atomic generation, incremental transaction, cursor, checkpoint, and migration tests;
- process-level crash/corruption/cancellation recovery;
- concurrent query while staging and strict project isolation;
- Clippy, formatting, locked build, and macOS/Windows/Linux storage checks.

### 13.4 End-to-end

- model-free live harness queries both tools before and after rename/reimport;
- disk truth, Godot API projection, stored graph, and MCP response are compared;
- editor and sidecar restart prove compatible cache reuse;
- session artifacts are removed cleanly and project resource bytes change only in the
  deliberate fixture mutation phase;
- normalized Windows/macOS results are semantically identical.

### 13.5 Required evidence artifacts

- `docs/codex-integration/INDEX-001-semantic-index-storage-and-migrations.md`;
- `docs/codex-integration/SPRINT-3-EVIDENCE.md`;
- `tests/codex/evidence/sprint-3-storage-spike.json`;
- `tests/codex/evidence/sprint-3-resource-graph-macos.json`;
- `tests/codex/evidence/sprint-3-resource-graph-windows.json`;
- canonical golden graph and diff report;
- migration/rebuild/corruption report;
- raw performance samples and summarized p50/p95;
- redacted protocol/query trace bound to commit and artifact hashes.

## 14. Risks and controls

| Risk | Control |
|---|---|
| Editor filesystem signals are coarse | Maintain an adapter manifest; compute bounded diffs; use UID for move detection |
| UID cache and file state disagree | Preserve both observations, mark diagnostic, and refuse silent rebind |
| Large projects stall the editor | Resumable main-thread slices, detached DTOs, lower-priority bulk lane, telemetry |
| Cycles or huge fan-out exhaust indexing/results | Store cycles without recursive expansion; deterministic ordering and limit/cursor bounds |
| Binary/imported fixtures are not reproducible | Generate through pinned Godot build and compare canonical semantic truth |
| Storage crash exposes partial state | Staging generation, transactional activation, fsync/locking tests, corruption quarantine |
| Cache from another project is accepted | Bind metadata/cursor/checkpoint to canonical `project_id` and schema version |
| Schema grows into Sprint 4 semantics | Resource-only entity/edge kinds; node/subresource identity remains explicitly deferred |
| Incremental event is lost | Exact resource revision; gap → invalidation → full resource snapshot |
| Logs expose local paths | Project-relative normalization, redaction scan, no raw DTO/value logging |

## 15. Definition of Ready

Implementation may begin because Sprint 2 snapshot/project/session foundations are
live-verified. Each work package still requires:

- a versioned input/output DTO or storage-neutral interface;
- explicit owner, thread boundary, limits, revisions, and cancellation point;
- positive, negative, and fault fixture;
- machine-readable expected evidence;
- compatibility/migration note for schema changes.

The open storage decision `D-05` is planned work, not a pre-sprint blocker. It becomes a
hard implementation blocker after day 5 if unresolved.

## 16. Sprint Definition of Done

Sprint 3 is complete only when:

- every parent roadmap work item and `S3-AC-01`–`S3-AC-12` has direct evidence;
- `INDEX-001` records `D-05`, schema versioning, migration, recovery, and rejected
  alternatives;
- Bridge RPC 1.2 schemas, C++ DTOs, Rust DTOs, and fixtures agree;
- the chosen persistent store passes atomicity, cancellation, migration, corruption,
  reopen, and concurrent-read tests;
- both MCP tools return the exact golden direct/reverse graph with evidence and no
  absolute paths;
- UID rename is incremental and stale UID behavior is explicit;
- focused and full Godot suites, Rust tests/Clippy/format, conformance, model-free live
  gates, and repository hooks pass;
- initial SLOs are met or a parent document is explicitly revised with measured
  evidence and accepted risk;
- Windows/macOS normalized evidence is archived and bound to the final commit;
- docs describe actual behavior, remaining limitations have owners, and no required
  gate is relabeled optional merely because it failed.

The next milestone after this Definition of Done is Sprint 4: `PackedScene`/`SceneState`
semantics and the node graph on top of the resource identity foundation.
