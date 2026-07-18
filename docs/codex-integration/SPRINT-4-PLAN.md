# Sprint 4 plan — scene semantics and node graph

**Status:** In progress — `S4-01/S4-02` complete locally; Bridge RPC 1.3 next

**Planned duration:** 10 working days

**Milestone:** Scene Understanding

**Baseline:** `cf310fbdab` (`codex/integration`), Sprint 3 accepted on macOS arm64 and Windows x86_64

**Parent scope:** [MASTER_SPRINT_ROADMAP.md](MASTER_SPRINT_ROADMAP.md), Sprint 4

**Normative scene contract:** [SCENE-001-scene-node-resource-model.md](SCENE-001-scene-node-resource-model.md)

## 1. Outcome

Sprint 4 extends the single persistent semantic index with a Godot-authored structural
scene graph. The bridge reads `PackedScene`/`SceneState`; the Rust sidecar normalizes,
persists, composes, and queries those observations. It does not parse `.tscn` as a
second semantic authority.

The sprint exposes two additional read-only MCP tools:

- `godot_get_scene_graph`;
- `godot_inspect_node`.

The sprint closes identity decision `D-06`, preserves the Sprint 3 resource graph,
and proves deterministic scene semantics on macOS arm64 and Windows x86_64. Linux and
remote CI are not Sprint 4 acceptance coordinates.

## 2. Scope boundaries

### 2.1 In scope

- scene, node-definition, node-occurrence, and built-in subresource identity;
- scene inheritance, instances, editable children, owners, groups, and connections;
- serialized local, inherited, and instance-override properties;
- attached scripts and Godot-reported exported property metadata;
- external and built-in resources referenced from a scene;
- animation track `NodePath` resolution;
- main scene, Autoload, InputMap, and semantic layer-name project context;
- Bridge RPC 1.3 scene snapshot/delta transfer and gap recovery;
- additive logical-index and physical segment-store migration;
- deterministic pagination, structured errors, recovery, SLO, and local evidence.

### 2.2 Explicitly out of scope

- raw `.tscn` parsing in the sidecar;
- GDScript/C# symbol or callable resolution, which belongs to Sprint 5;
- general find-usages/evidence aggregation, which belongs to Sprint 6;
- unsaved dirty-scene overlay merge, which belongs to Sprint 7;
- runtime nodes, debugger state, writes, transactions, or Undo;
- Linux and remote CI gates.

## 3. Work breakdown

| ID | Deliverable | Exit condition |
|---|---|---|
| `S4-01` | `SCENE-001` and D-06 identity vectors | Every identity scope and fallback reproduces exactly in Python and Rust |
| `S4-02` | Scene fixture and independent golden oracle | Inheritance, instances, overrides, signals, groups, animations, resources, and project context are exact and order-independent |
| `S4-03` | Bridge RPC 1.3 schema and negotiation | 1.3 scene profile passes cross-language conformance; 1.0–1.2 remain compatible |
| `S4-04` | `SceneStateAdapter` and bounded scene journal | Full snapshot, delta, change notification, and gap invalidation stay inside the 2 ms main-thread budget |
| `S4-05` | Rust scene wire client and normalizer | Strict DTO validation, canonical identities, values, relations, diagnostics, and graph digest pass fixtures |
| `S4-06` | Logical schema 1.2 and `segment-v2` | Existing resource data migrates; scene shards recover atomically after cancellation/crash/corruption |
| `S4-07` | Semantic coordinator and composition | Resource and scene freshness are independent; inheritance/instance closure activates atomically |
| `S4-08` | Two scene MCP tools | Query contract, one-generation pagination, cursor isolation, partial results, and redaction pass |
| `S4-09` | macOS and Windows live gates | Both hosts pass the same source freeze, mutations, recovery matrix, and SLOs |
| `S4-10` | Deterministic final aggregate | `S4-AC-01`–`S4-AC-12` pass; docs and raw evidence hashes match the aggregate |

The contract/oracle boundary is locally frozen by
`tests/codex/evidence/sprint-4-stage-1-contracts.json`. It validates strict Draft
2020-12 bundles and D-06 vectors in Python/Rust, verifies fixture hashes and reference
closure, and loads every text-scene coordinate through the local Godot 4.8 editor.

## 4. Required data flow

1. `EditorFileSystem`/resource revision identifies a scene that may have changed.
2. `SceneStateAdapter` loads through Godot resource APIs and emits detached observations.
3. Bridge RPC 1.3 transfers a complete scene snapshot or an exact ordered delta.
4. The sidecar validates and normalizes observations without reading scene text.
5. The single semantic-index writer activates immutable resource and scene shards.
6. Scene tools pin exactly one current generation for the entire request.

A changed scene makes only the scene domain non-current until its scene delta commits.
Sprint 3 resource tools remain usable when their resource-domain checkpoint is current.
A journal gap, incompatible checkpoint, corrupt generation, or failed composition forces
a full scene resnapshot; stale scene facts are never served as current.

## 5. Public interfaces

Bridge RPC 1.3 adds capabilities `scene.packed_state`, `scene.incremental_index`, and
`scene.project_context`; methods `scene.snapshot.get` and `scene.delta.get`; domain
`scene_graph`; global `scene_graph_revision`; and scene change/gap notifications.

`godot_get_scene_graph` accepts `scene`, optional `limit` (default 50, range 1–200),
and optional `cursor`. It returns a composed, bounded node page with definition/origin,
parent/owner, instance chain, groups, connection summaries, and diagnostics.

`godot_inspect_node` accepts exactly one selector: opaque `node_id`, or `scene` plus a
relative node-only `node_path`. It accepts the same limit/cursor contract and returns
property values with declaring source and `local`, `inherited`, or `instance_override`
origin, attached script, resources, groups, connections, and animation references.

Both tools return the common project/generation/revision/freshness/evidence envelope.
Cursors are HMAC-authenticated, expire after at most five minutes, and bind the project,
tool, selector, limit, generation, index revision, scene graph revision, and offset.

## 6. Acceptance criteria

| ID | Requirement |
|---|---|
| `S4-AC-01` | Saved node identity survives rename, reparent, save, and reopen; duplication receives a distinct identity |
| `S4-AC-02` | Built-in subresource identity survives section reorder/save/reopen where Godot provides a scene-unique ID; fallbacks are explicitly content-revision scoped |
| `S4-AC-03` | Effective properties identify the correct local, inherited, or instance-override declaring source |
| `S4-AC-04` | Nested instances trace to source scenes and distinguish editable, owned, and internal observations without inventing runtime entities |
| `S4-AC-05` | Signal and group facts identify their exact source node, target node where applicable, and declaration scope |
| `S4-AC-06` | Every animation track `NodePath` is resolved to a node entity or marked deterministically broken/unresolved |
| `S4-AC-07` | Attached scripts, serialized exported properties, external/subresources, main scene, Autoload, InputMap, and allowlisted layer names match the oracle |
| `S4-AC-08` | Semantically equivalent text scenes with different valid section order produce the same normalized graph digest |
| `S4-AC-09` | Full/incremental ingestion, dependent-scene propagation, reconnect, journal gap, cancellation, and corruption never publish a partial scene generation |
| `S4-AC-10` | `segment-v1` migrates to `segment-v2` without changing resource query results; a migration failure preserves the prior generation |
| `S4-AC-11` | Both MCP tools pass strict schema, selector, pagination, cursor, partial-result, error, limits, and redaction tests |
| `S4-AC-12` | macOS and Windows normalized graph digests match and all Sprint 4 SLOs pass on one source freeze |

## 7. Performance and evidence gates

- cached scene query p95 is at most 300 ms;
- scene change to query visibility p95 is at most 2 seconds;
- status/ping during bulk scene indexing p95 is at most 200 ms;
- the bridge records zero main-thread samples above 2 ms;
- no absolute filesystem path, token, imported payload path, or raw project content is
  emitted in ordinary diagnostics or evidence metadata;
- macOS and Windows evidence is produced locally from one source freeze and installed
  only after receipt, schema, digest, and deterministic aggregate validation;
- final evidence records `remote_ci: not_run`.

## 8. Commit boundaries

1. Contract, D-06 vectors, fixture, and oracle.
2. Bridge RPC 1.3 schemas and negotiation.
3. C++ `SceneStateAdapter`, projection, and journal.
4. Rust scene wire client and normalization.
5. Store schema, `segment-v2`, and migration.
6. Coordinator, composition, invalidation, and recovery.
7. MCP tools and cursor contract.
8. macOS live gate and runner freeze.
9. Windows evidence, final aggregate, and completion documentation.
