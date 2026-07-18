# INDEX-001 — semantic index contract, storage, and migrations

**Status:** Resource contract frozen; local `D-05` selects the segment store

**Version:** 0.1

**Frozen scope:** Sprint 3 resource entities and direct/reverse file dependencies

**Parent:** [SPRINT-3-PLAN.md](SPRINT-3-PLAN.md)

**Stage evidence:**
[SPRINT-3-STAGE-1-PLAN.md](SPRINT-3-STAGE-1-PLAN.md), `S3-01`/`S3-02`;
[SPRINT-3-STAGE-2-PLAN.md](SPRINT-3-STAGE-2-PLAN.md), `S3-03`/`D-05`

## 1. Purpose and normative language

This document is authoritative for resource identity, the storage-neutral resource
graph, index revisions/generations, invalidation, migration guarantees, and direct
resource queries. `PROTOCOL-001` remains authoritative for transport and project/session
identity. `PRODUCT-001` remains authoritative for product identity scopes and evidence.

The terms **MUST**, **MUST NOT**, **SHOULD**, and **MAY** are normative. A producer or
store that cannot satisfy a MUST reports the defined diagnostic or invalidation; it does
not guess a result and label it exact.

`D-05` selects the measured immutable segment store for the local implementation stream.
Table names, SQL, files, pages, segments, and backend cursors remain absent from the
public contract. The chosen backend MUST implement the observable guarantees below
without changing entity IDs, graph meaning, ordering, or recovery behavior.

## 2. Scope and non-goals

In scope:

- project resources known to `EditorFileSystem`/`ResourceLoader`;
- valid and missing `ResourceUID` identity;
- normalized project-relative paths, Godot type, import state, mtime, byte size, and
  bounded SHA-256;
- declared direct dependencies and derived direct reverse owners;
- full generations, incremental batches, tombstones, diagnostics, invalidation,
  compatible reopen, and migration;
- `.tres`, `.res`, `.tscn`, `.scn`, and imported source assets.

Out of scope:

- scene nodes, instances, inheritance, overrides, or signal semantics;
- subresource persistent identity (`D-06`/Sprint 4);
- GDScript/C# symbols and dynamic loads;
- recursive/transitive graph queries;
- dirty editor overlays, runtime facts, or write transactions;
- exposing `.godot/imported` payloads as project resources.

## 3. Terminology ledger

| Term | Normative definition | Scope/lifetime |
|---|---|---|
| Resource entity | One file-backed Godot resource addressable by UID or fallback path/content identity | Persistent for UID; content revision for fallback |
| Display path | Normalized `res://` spelling returned to a user | Current generation |
| Comparison path | NFC-normalized path key used for equality/order without case folding | Current generation |
| Content generation | `sha256:` plus 64 lowercase hex digits for the bounded source/resource bytes | Changes with bytes |
| Direct dependency | One declaration reported by Godot from a source resource to a UID/path reference | Resource revision |
| Reverse owner | Derived lookup from target to sources over the same committed direct edge set | Index revision |
| Resource revision | Strictly increasing editor-session counter for resource catalog observations | Editor session |
| Project revision | Existing Bridge RPC project-wide session counter | Editor session |
| Index revision | Strictly increasing durable commit counter owned by one project store | Store lifetime |
| Generation | Immutable, queryable graph snapshot activated atomically | Persistent until retired |
| Checkpoint | Last complete editor/resource observation durably represented by an index revision | Persistent |
| Tombstone | Bounded deletion record retained while affected inbound edges/diagnostics reconcile | One or more generations per retention rule |
| Exact result | Result derived from one complete active generation and authoritative source rules | Bound to index revision |
| Partial result | Bounded result whose unresolved references are returned with diagnostics | Bound to index revision |

`entity_id`, `edge_id`, `generation_id`, and cursors are opaque to clients. Their
canonical construction is specified so independent producers can reproduce them, not so
clients can parse them.

## 4. Source authority

| Fact | Authority | Required behavior |
|---|---|---|
| UID and current UID-to-path mapping | `ResourceUID` | UID determines persistent resource identity; path is mutable metadata |
| File/type/import visibility | `EditorFileSystem` | The sidecar MUST NOT infer successful import from extension alone |
| Direct dependencies | `ResourceLoader::get_dependencies` plus editor filesystem observation | Exact only for the declarations Godot reports |
| Source bytes | File below the physical canonical project root | Hash proves bytes only; traversal/symlink/reparse escape is rejected |
| Reverse owners | Committed direct edge set | Derived at the same index revision; never independently ingested |

When authorities disagree, observations are preserved in safe bounded fields and a
diagnostic is committed. The index MUST NOT repair a UID/path disagreement with filename,
extension, equal-content, or last-seen heuristics.

Imported source assets are entities at their source `res://` path. Importer/type/status
metadata may name a project-relative source path, but internal `.godot/imported` payload
paths and bytes MUST NOT appear as separately queryable entities, ordinary result fields,
or ordinary logs.

## 5. Canonical paths

### 5.1 Accepted input

A canonical resource path:

1. is valid UTF-8 and starts with the exact lowercase prefix `res://`;
2. uses `/` only; `\`, NUL, control characters, URI query/fragment syntax, and an
   additional scheme are rejected;
3. contains at least one non-empty segment after the prefix;
4. collapses repeated `/` and removes `.` segments;
5. rejects every `..` segment even if lexical resolution would remain below the root;
6. rejects a trailing `/` for a resource file;
7. preserves the normalized display spelling and computes an NFC comparison key;
8. does not case-fold.

An absolute filesystem path is never accepted at a public/resource query boundary. When
the sidecar maps a canonical `res://` path to disk, it joins against the physical
canonical project root and verifies after resolution that the target has not escaped via
symlink, junction, mount alias, or reparse point.

### 5.2 Collisions

Two display paths with the same NFC comparison key are a `path_normalization_collision`.
Two paths that differ only by case remain distinct logical keys, but if the active
filesystem aliases them, both are invalidated with `nonportable_case_collision`; neither
receives an exact fallback identity.

Canonical path vectors live in
`tests/codex/fixtures/resource_graph_oracle/identity-vectors.json`.

## 6. Resource identity

### 6.1 Canonical UID text

A UID is valid only when `ResourceUID::text_to_id` returns a non-negative ID and
`ResourceUID::id_to_text` returns its canonical `uid://` spelling. Producers MUST use
that canonical spelling for identity input and output. A syntactically valid UID that is
not mapped to existing project bytes may still be preserved as a dependency reference;
it is not a resource entity by itself.

### 6.2 UID-backed entity ID

The v1 UID-backed algorithm is:

```text
domain = UTF8("godot-codex/resource-entity/uid/v1") || 0x00
input  = domain || UTF8(canonical_uid)
digest = SHA-256(input)
entity_id = "godot:resource:uid:v1:" || BASE64URL_NOPAD(digest)
```

Its `identity_scope` is `persistent` and `identity_strength` is `resource_uid`. Content
edits, reimport, delete/re-add, and rename do not change the entity ID while the canonical
UID is the same.

### 6.3 Path/content fallback entity ID

A UID-less valid file MUST have a completed content generation before an exact fallback
entity is committed:

```text
content_generation = "sha256:" || LOWER_HEX(SHA256(bounded_file_bytes))
domain = UTF8("godot-codex/resource-entity/path-content/v1") || 0x00
input  = domain || UTF8(comparison_path) || 0x00 || UTF8(content_generation)
digest = SHA-256(input)
entity_id = "godot:resource:path-content:v1:" || BASE64URL_NOPAD(digest)
```

Its `identity_scope` is `content_revision` and `identity_strength` is
`path_content_generation`. Rename or byte change produces a new entity ID. Equal bytes
at two paths produce different IDs. If bounded hashing fails or observes a concurrent
size/mtime change, the record is `invalid`, has `hash_unavailable`, and MUST NOT be
returned as an exact fallback entity until requeued hashing succeeds.

### 6.4 Collision defense

The store retains the canonical identity input alongside the entity ID. If one ID is
observed with different canonical input, activation fails with `entity_id_collision`,
the staging generation is quarantined, and the previous active generation remains
queryable. Implementations MUST NOT resolve a digest collision by overwriting a record.

### 6.5 Identity transition table

| Event | UID-backed result | UID-less result |
|---|---|---|
| Rename/move | Same entity ID, new current path, one move/upsert transition | Remove old entity and upsert new entity |
| Content edit | Same entity ID, new content generation | Remove old entity and upsert new entity |
| Delete | Bounded tombstone retains UID/entity ID while inbound edges reconcile | Tombstone retains prior scoped ID only for reconciliation |
| Re-add same bytes/path | Same UID entity ID | Same deterministic fallback ID, but no persistence claim across missing interval |
| Re-add different bytes | Same UID entity ID | New fallback entity ID |
| Reimport | Same source UID entity ID, new import/hash metadata | Fallback changes only if source bytes change |

A UID-preserving move MUST NOT increase `full_rebuild_count`. A UID-less move MUST NOT be
inferred from equal content.

## 7. Logical records

All records carry `schema_version`, `project_id`, `index_revision`, and safe bounded
strings. Absolute paths and source/import payloads are forbidden.

### 7.1 `IndexMetadata`

- semantic schema version and reader/writer compatibility range;
- project ID and hash algorithm identifiers;
- active generation ID and index revision;
- build state: `empty`, `building`, `ready`, `invalidated`, or `recovering`;
- last compatible checkpoint and bounded recovery reason;
- counters including full rebuilds, incremental commits, quarantine events, and failed
  migrations.

### 7.2 `ResourceEntity`

- entity ID, canonical UID or explicit `uid_missing`;
- display/comparison path and identity scope/strength;
- Godot resource type, `source`/`imported_source` marker, and import state;
- content generation, mtime, byte size, optional SHA-256, and hash status;
- validity: `valid`, `partial`, `invalid`, `deleted`;
- authoritative resource revision.

### 7.3 `SourceDocument`

- entity ID and project-relative display/comparison path;
- observed mtime/size before and after hashing;
- content generation/hash and ingest status;
- importer/type metadata without generated payload paths.

### 7.4 `DependencyEdge`

- opaque deterministic edge ID;
- source entity ID;
- target canonical UID and fallback path, or path-only target;
- resolved target entity ID/current path when available;
- relation `references` and optional Godot-declared target type;
- resolution: `resolved`, `missing`, or `stale_uid`;
- authority `godot_resource_loader` and resource revision.

Duplicate Godot declarations with the same source and canonical target reference are
deduplicated. Target UID wins the edge key when present; its fallback path remains
evidence and does not change edge identity during rename. Path-only edges key by
comparison path.

The v1 edge ID algorithm is:

```text
domain = UTF8("godot-codex/resource-edge/references/v1") || 0x00
kind   = "uid" when target UID is present, otherwise "path"
target = canonical UID when present, otherwise target comparison path
input  = domain || UTF8(source_entity_id) || 0x00 || UTF8(kind) || 0x00 || UTF8(target)
edge_id = "godot:edge:references:v1:" || BASE64URL_NOPAD(SHA-256(input))
```

Consequently, a UID-backed target rename does not change the edge ID; a path-only target
rename does. A source with fallback identity produces new edge IDs whenever its source
entity ID changes.

### 7.5 `Diagnostic`

- stable diagnostic ID derived from code, affected entity/reference, and first observed
  content generation;
- code, severity, safe bounded details, first/last observed revisions;
- status `active` or `resolved` and optional resolving index revision.

### 7.6 `IngestionCheckpoint` and `IndexGeneration`

The checkpoint stores editor session ID, resource revision, project revision, durable
index revision, source completeness, and last snapshot checksum. A generation stores its
opaque ID, parent ID, schema version, state, creation reason, validation digest, and
activation revision.

## 8. Dependency and diagnostic rules

| Observation | Edge resolution | Required diagnostic |
|---|---|---|
| UID resolves to existing indexed entity | `resolved` | None |
| Path-only target exists and is indexed | `resolved` | None |
| Path-only target does not exist | `missing` | `missing_dependency` |
| UID is valid but absent/unmapped/mapped to missing bytes | `stale_uid` | `stale_resource_uid` |
| UID maps to one path while fallback names another existing resource | `stale_uid` until reconciled | `resource_uid_path_mismatch` |
| Two resources claim one UID | Neither is exact | `duplicate_resource_uid` and generation invalidation |
| Hash cannot be produced safely | Entity partial/invalid | `hash_unavailable` |
| Import sidecar exists but editor reports invalid import | Entity partial | `invalid_import` |

Missing and stale edges remain in the committed graph so callers can explain an
unresolved reference. They are never rebound by basename or content hash. Resolved
diagnostics remain for one subsequent committed generation, then may be pruned; active
tombstones remain until all inbound edges from the deletion generation have reconciled,
with a hard retention ceiling configured by the store and reported in metadata.

## 9. Revisions and atomic batches

### 9.1 Counters

- `resource_revision` and `project_revision` are unsigned 64-bit, strictly increasing
  inside one editor session, and reset only with a new session ID.
- `index_revision` is unsigned 64-bit, persists with the store, advances exactly once
  per successful activation/incremental commit, and never advances for cancelled/failed
  work.
- `generation_id` is an opaque collision-resistant 128-bit-or-stronger value and is not
  ordered.
- `content_generation` is the SHA-256 form defined in section 6.3.

### 9.2 Incremental precondition

An incremental batch includes project ID, editor session ID, exact
`previous_resource_revision`, next resource revision, and source completeness. It may be
applied only when project/session match and the active checkpoint equals the exact
previous revision. Duplicate already-committed batches are idempotently acknowledged by
batch identity; overlapping or skipped revisions emit `resource_journal_gap`, invalidate
incremental freshness, and require a full resource snapshot.

### 9.3 Atomic commit set

One commit atomically changes:

- resource/source upserts and removals;
- direct edges and derived reverse entries;
- diagnostics and tombstones;
- checkpoint and generation metadata;
- index revision and counters.

Queries before commit observe the previous active revision; queries after commit observe
the new complete revision. No query may observe staging records or a mixture of direct
and reverse revisions.

## 10. Generation state machine and recovery

```text
absent -> staging -> validating -> active -> retired
                    |          |
                    +-> quarantined
staging/validating -> cancelled
active + journal gap -> invalidated -> staging(full snapshot)
```

- Full build creates a new staging generation without mutating active data.
- Validation covers schema, identity-input uniqueness, direct/reverse parity, project
  binding, checkpoint, and generation digest.
- Activation is one durable transaction or atomic pointer swap after required flushes.
- Cancellation/crash before activation leaves the old generation active.
- Startup discards or quarantines incomplete staging work, validates active metadata,
  and reuses a compatible active generation when project/schema/checkpoint agree.
- Corrupt or incompatible active data is never returned as current. It is quarantined
  and rebuilt from Godot; project resource bytes are never changed by recovery.
- Queue overflow or journal gap invalidates freshness explicitly and triggers a bounded
  full resnapshot. Structural changes are never silently dropped.

## 11. Schema compatibility and migration

The semantic schema version is `{major, minor}`.

- A reader accepts the same major and a minor within the generation's declared reader
  compatibility range.
- Additive optional fields may default only when the default cannot change identity,
  edge resolution, diagnostic meaning, ordering, or completeness.
- Removing/renaming required fields or changing any canonical algorithm, enum meaning,
  atomicity rule, or ordering requires a new major and staged migration/rebuild.
- An unknown major is incompatible and never queried as current.

Migration always creates a new staging generation. It is restartable and idempotent for
the same source generation and target schema, validates all invariants, and activates
only after durable success. Cancellation, crash, or failed validation preserves the
prior generation. If no safe migration exists, the cache is quarantined and rebuilt.

### 11.1 `D-05` physical segment-store decision

`D-05` freezes the measured current candidate layout as production physical schema
`segment-v1`. It lives at `.godot/codex/index/` and uses immutable, content-addressed
`.seg` files plus sorted offset indexes. Resources, source documents, direct edges,
reverse edges, diagnostics, tombstones, and the materialized resource lookup are
partitioned into 256 stable shards by the first byte of a SHA-256 key. Generation
manifests bind the project, logical schema, physical version, checkpoint, validation
digest, and every referenced segment digest.

Logical generation IDs remain protocol values such as `generation:sha256:<hex>`, but
physical generation, staging, and commit artifact names use
`generation-<lowerhex-SHA256(UTF-8 generation_id)>`. This keeps the layout identical
and filename-safe on Windows, macOS, and Linux without changing query-visible identity.
Readers prefer the portable name and may fall back only to a legacy raw-ID artifact
whose ID contains ASCII alphanumerics, `:`, `-`, or `_`; path separators and other
unsafe legacy names are never resolved.

One writer holds `.godot/codex/index.lock`; a competing writer receives `store_busy`.
Data and indexes are written as new files and flushed before their staging manifest is
renamed into `generations/`. Activation writes a new uniquely named commit marker only
after the generation manifest and header are durable. Readers choose the latest valid
marker, verify the manifest digest and every referenced segment, and therefore observe
only the complete old or complete new generation. Retention keeps the active and one
retired generation and removes unreferenced immutable segments.

Startup ignores incomplete staging state. A corrupt marker, manifest, segment, lookup,
project binding, or incompatible physical version is never returned as current and is
quarantined/rebuilt without changing project resources. Inside the test-only spike,
`physical_version=1` is the legacy migration fixture that resolves resource lookups from
primary records, while the measured candidate is `physical_version=2`. Its idempotent
v1-to-v2 migration creates a new generation with materialized lookup shards, validates
an identical logical graph, and activates it through the same marker protocol. The
production implementation starts its own version namespace at `segment-v1` rather than
shipping the deliberately obsolete spike fixture.

Bundled SQLite also passed all local correctness gates, but it was rejected for this
decision because its measured rename write amplification (`23,438.15x` versus
`316.07x`), bounded artifact size (`212.38 MiB` versus `45.49 MiB`), rename p95
(`1,618.29 ms` versus `478.45 ms`), and dependency/binary impact were materially worse.
The segment store's slower full build did not outweigh those results under the frozen
weighted rule. Windows/Linux process-level portability remains `not_run`; it is a
separate later validation gate, not remote-CI evidence for this local decision.

### 11.2 `segment-v2` scene-domain extension

Sprint 4 advances the logical schema from 1.1 to 1.2 and the production physical
format from `segment-v1` to `segment-v2`. The resource, source-document, direct,
reverse, diagnostic, tombstone, and resource-lookup shard contracts are unchanged.
`segment-v2` adds independent content-addressed shard maps for scene definitions,
nodes, properties, structural relations, signal connections, group memberships,
animation references, and materialized scene/node lookup keys.

The logical generation contains an independently checkpointed scene domain with its
editor session, resource revision, scene graph revision, completeness flag, normalized
snapshot checksum, and semantic validation digest. Scene records retain authority and
both resource/scene revision coordinates. Public semantic IDs never expose physical
segment digests or offsets.

Migration reads and validates the active `segment-v1` resource generation, advances
its logical schema to 1.2, creates an intentionally empty non-current scene domain,
and writes a new `segment-v2` manifest. Unchanged resource segments and lookup shards
are reused by digest. Activation still occurs only through a durable commit marker;
cancellation, process death, validation failure, or corruption leaves the prior
generation active. The migration is idempotent once `segment-v2` is current and a
restart removes incomplete staging artifacts before retrying.

## 12. Direct query contract

### 12.1 Lookup

Callers may select one resource by opaque entity ID, canonical UID, or canonicalizable
`res://` path. Supplying zero/multiple selectors is `invalid_query`. A path/UID that maps
to multiple invalid records returns `ambiguous_resource`; it never chooses one.

Sprint 3 supports only:

- direct declared dependencies of one source;
- direct owners whose edge resolves or refers to one target.

No recursive expansion is performed.

### 12.2 Ordering, limits, and cursors

Results order by target/source comparison path, then canonical UID (missing sorts last),
then entity ID. Diagnostics order by code then diagnostic ID.

- default page size: 50;
- hard page size: 200;
- hard path size: 1,024 UTF-8 bytes after normalization;
- hard safe diagnostic detail: 1,024 UTF-8 bytes;
- cursor lifetime: at most 5 minutes and never beyond retirement of its generation.

A cursor is authenticated/opaque and bound to project ID, query kind, normalized
selector, filters, page size, generation ID, and index revision. A mismatch, expiry,
sidecar restart, or retired revision returns `stale_cursor`; a cursor never resumes
against a newer generation.

### 12.3 Completeness and errors

Resolved and unresolved direct edges may be returned together. Missing/stale targets
produce a partial result with their structured diagnostics. `exact` is allowed only when
the active generation is complete for the requested domain. Other stable errors are
`invalid_path`, `resource_not_found`, `index_not_ready`, `index_not_current`,
`capability_unavailable`, `stale_cursor`, `result_limit_exceeded`, and
`project_not_bound`.

## 13. Threading, hashing, and security boundaries

- `ResourceUID`, `EditorFileSystem`, and editor singletons run only on the Godot main
  thread inside the adapter and the existing 2 ms slice budget.
- Detached DTOs cross to worker/sidecar code; storage, hashing, serialization, and IPC
  do not retain Godot object pointers.
- Raw-file hashing runs on a bounded worker. Size and mtime are checked before and after;
  a concurrent change discards the hash and requeues it.
- Bulk queues are bounded and lower priority than control/status/live-context traffic.
- Project root, absolute paths, tokens, imported payload paths, source bytes, and raw DTO
  payloads are absent from ordinary logs and semantic result objects.

## 14. Canonical examples

### 14.1 UID-preserving rename

Before:

```text
uid=uid://b
path=res://resources/shared_leaf.tres
entity=godot:resource:uid:v1:9yWqfTIjHm9QQNXjsLoOwef_VTZRuXNhgO46kfXgs-4
```

After:

```text
uid=uid://b
path=res://resources/renamed/shared_leaf_renamed.tres
entity=godot:resource:uid:v1:9yWqfTIjHm9QQNXjsLoOwef_VTZRuXNhgO46kfXgs-4
```

Every edge keyed by UID retains its edge identity while its resolved current path and
reverse lookup metadata update in the same commit.

### 14.2 UID-less equal content

`twin_a.tres` and `twin_b.tres` have identical bytes and content generation but distinct
fallback IDs because their comparison paths differ. Renaming `twin_a.tres` creates a
third ID. Exact inputs/outputs are in the identity vector bundle.

### 14.3 Missing versus stale

`res://missing/not_created.tres` without a UID is `missing_dependency`. Canonical
`uid://0` with fallback `res://missing/stale_target.tres` is `stale_resource_uid`. Both
edges remain observable; neither resolves to a similarly named file.

## 15. Decision register

| Decision | State | Deadline |
|---|---|---|
| Resource identity/path/content algorithms | Frozen by `S3-01` | Complete |
| Logical graph/revision/generation/query contract | Frozen by `S3-01` | Complete |
| Golden fixture/oracle format | Frozen by `S3-02` | Complete |
| `D-05` persistent backend and physical migration | Segment store selected by full local macOS matrix | Complete locally; Windows/Linux portability `not_run` |
| `D-06` node/subresource persistent identity | Frozen by `SCENE-001`; implementation/evidence in progress | Sprint 4 |

### 15.1 `S3-03` execution status

The storage-neutral crate, bundled-SQLite candidate, immutable segment candidate, and
the shared benchmark/fault harness are implemented. Full macOS evidence on 2026-07-15
passes all 12 correctness, recovery, migration, locking, packaging, stress, and SLO
gates plus all 13 detailed hard-kill/cancellation/corruption coordinates for both
candidates. Under the frozen weighted rule the local score is `0.3508`
(`95% CI 0.3474–0.3556`) for SQLite and `0.8122` (`95% CI 0.8118–0.8138`)
for the segment store.

The canonical local report at `tests/codex/evidence/sprint-3-storage-spike.json` records
`chosen_backend=segment`, the source-tree digest, every raw sample, and the deterministic
decision reason. This freezes `D-05` for subsequent local Sprint 3 implementation.
Windows/Linux results have not been produced and are not implied by this decision.
Exact commands, candidate layouts, raw metrics, and the remaining portability boundary
are recorded in [SPRINT-3-STAGE-2-PLAN.md](SPRINT-3-STAGE-2-PLAN.md).

## 16. Contract acceptance

The resource contract is frozen when:

- Draft 2020-12 schemas validate every canonical oracle and reject every named negative;
- UID/path-content vectors reproduce exactly;
- the Godot API probe matches the canonical base and mutation graph facts;
- direct/reverse parity, missing/stale distinction, formats, rename, delete/re-add,
  reimport, content edit, and invalidation are covered;
- two fixture restores are byte-identical and leave tracked source files unchanged;
- storage/Bridge/MCP implementation remains outside this gate and cannot redefine the
  frozen logical behavior.

Evidence and exact commands are recorded in
`tests/codex/evidence/sprint-3-stage-1-contracts.json` and the Stage 1 completion section
of [SPRINT-3-STAGE-1-PLAN.md](SPRINT-3-STAGE-1-PLAN.md).
