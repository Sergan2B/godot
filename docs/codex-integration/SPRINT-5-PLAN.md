# Sprint 5 plan — GDScript symbols and program relations

**Status:** In progress — `S5-01` and `S5-02` are complete and locally verified
on macOS arm64; `S5-03` is the next gate; full Sprint 5 acceptance is not claimed

**Planned duration:** 10 working days

**Milestone:** Script Semantics

**Baseline:** `73ec8ade7897109b8fed2e6fcb714a1ad01dd95b`
(`codex/integration`), Sprint 4 accepted on one macOS arm64/Windows x86_64
source freeze

**Parent scope:** [MASTER_SPRINT_ROADMAP.md](MASTER_SPRINT_ROADMAP.md), Sprint 5

**Normative parents:**
[PRODUCT-001-semantic-bridge-vision-and-plan.md](PRODUCT-001-semantic-bridge-vision-and-plan.md),
[ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md](ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md),
[PROTOCOL-001-bridge-rpc-v1.md](PROTOCOL-001-bridge-rpc-v1.md),
[INDEX-001-semantic-index-storage-and-migrations.md](INDEX-001-semantic-index-storage-and-migrations.md),
[MCP-001-project-scoped-read-tools.md](MCP-001-project-scoped-read-tools.md), and
[SCENE-001-scene-node-resource-model.md](SCENE-001-scene-node-resource-model.md)

**Normative script contract:**
[SCRIPT-001-gdscript-and-csharp-adapters.md](SCRIPT-001-gdscript-and-csharp-adapters.md)

## 1. Outcome

Sprint 5 extends the one persistent semantic index with saved-script declarations,
program relations, diagnostics, and source evidence. Godot's GDScript
parser/analyzer is the semantic authority. The bridge projects bounded detached
observations; the Rust sidecar validates, normalizes, persists, composes, and
queries those observations. The sidecar does not implement a second GDScript
parser and does not infer an `exact` target from text matching.

The sprint exposes two additional read-only MCP tools:

- `godot_search_symbols`;
- `godot_inspect_symbol`.

The resulting graph connects scripts to Sprint 3 resources and Sprint 4 scene
nodes while preserving independent resource, scene, and script freshness. It
closes architecture decision `D-07`, advances Bridge RPC through a compatible
1.4 profile, and proves deterministic saved-script semantics on macOS arm64 and
Windows x86_64. Linux and remote CI are not Sprint 5 acceptance coordinates.

## 2. Scope boundaries

### 2.1 In scope

- saved `.gd` source documents known to the Godot editor;
- script, named/inner class, method/function, property, constant, enum, enum
  member, signal, parameter, and local declaration observations;
- GDScript inheritance, method overrides, and base-declaration relations;
- analyzer-resolved symbol references and callable targets;
- literal `preload()`/`load()` paths and UIDs, including resolved resource links;
- dynamic calls, dynamic loads, and string `NodePath` observations without false
  `exact` classification;
- attached-script relations from saved scene definitions to script symbols;
- content-bound source evidence with deterministic file/range coordinates;
- parser/analyzer errors and warnings without project-wide indexing failure;
- Bridge RPC 1.4 script snapshot/delta transfer and gap recovery;
- additive logical-index and physical segment-store migration;
- minimal C# discovery/diagnostics abstraction without a new Roslyn dependency;
- a clean bridge build and honest `capability_unavailable` state when the
  GDScript module or optional C# language service is absent;
- deterministic symbol queries, pagination, structured errors, recovery, SLOs,
  redaction, and local macOS/Windows evidence.

Parameters and local declarations may be indexed as content-revision-scoped
targets when the analyzer resolves them. They are not promoted to persistent
project symbols and are excluded from default project-wide search results.

### 2.2 Explicitly out of scope

- an independent GDScript parser or type checker in the sidecar;
- dependence on a running external LSP connection or a user-enabled language
  server setting for correctness;
- unsaved script-buffer overlays, active script, or editor selection, which
  belong to Sprint 7;
- general reverse `godot_find_usages` aggregation, which belongs to Sprint 6;
- heuristic whole-project text search presented as semantic references;
- dynamic-dispatch proof without runtime evidence;
- deep C# symbol/reference analysis or a bundled Roslyn service;
- runtime stack/source mapping, writes, script patches, transactions, or Undo;
- Linux and remote CI gates.

## 3. Decisions and authority boundaries

### 3.1 D-07: analyzer integration

`S5-01` must compare two implementations against the same oracle:

1. read-only reuse of compatible `ExtendGDScriptParser`/`GDScriptWorkspace`
   cache entries at an exact saved content revision;
2. a bridge-owned cache over the existing GDScript parser/analyzer APIs.

The decision records correctness, cold/warm latency, incremental latency, peak
memory, invalidation behavior, parse-error behavior, and editor main-thread cost.
An active LSP workspace may be reused only when its content hash and analyzer
configuration match the saved source revision. Dirty LSP buffers must never be
published as saved static state. If no compatible cache exists, the adapter must
still work without starting or connecting to an external language server.

Whichever candidate wins, `SCRIPT-001` freezes one shared semantic projection
contract rather than exposing LSP DTOs directly. The losing candidate and the
reasons for rejection are recorded with the spike evidence.

### 3.2 Identity and source coordinates

`SCRIPT-001` freezes symbol identity and range vectors, while `S5-01` closes
D-07 before production ingestion begins. The intended scopes are:

- named script/class/member symbols: script resource identity, language, and a
  canonical qualified analyzer key; body edits or line movement alone do not
  change identity;
- parameters, locals, lambdas, unresolved observations, and diagnostics:
  content-revision identity bound to the exact source hash and range;
- a rename or semantic owner change creates a new symbol identity unless the
  Godot analyzer exposes a stronger stable key that is proven by vectors.

Every source location is bound to a normalized `res://` path, exact source
SHA-256, and half-open UTF-8 byte span. Human-readable line/column coordinates
are derived fields with their encoding declared by the contract. LF/CRLF,
Unicode, combining characters, astral characters, and tabs receive explicit
cross-language test vectors; a range is never interpreted against a different
content hash.

### 3.3 Confidence and freshness

- `exact` is emitted only for an unambiguous parser/analyzer result or a
  structurally exact Godot resource/scene relation.
- `dynamic` identifies a syntactically known relation whose target cannot be
  resolved statically, such as `call(variable)` or `load(variable)`.
- `probable` is allowed only when `SCRIPT-001` defines the bounded inference and
  evidence. It is never merged into an `exact` result.
- freshness is independent of confidence. A previously exact fact from an older
  script revision is stale, not current and weaker.
- parse failure may produce partial current diagnostics, but stale symbols from
  an older content hash are never returned as current.

## 4. Work breakdown

| ID | Deliverable | Exit condition |
|---|---|---|
| `S5-01` | `SCRIPT-001`, D-07 spike, identity/range/confidence vectors | One analyzer integration is selected; saved/dirty separation, symbol identity, ranges, and confidence reproduce in C++/Python/Rust |
| `S5-02` | Script fixture and independent golden oracle | Typed, untyped, dynamic, inherited, broken, Unicode, GDScript, C#, and scene-attachment cases validate without implementation-derived expectations |
| `S5-03` | Bridge RPC 1.4 schemas and negotiation | The 1.4 script profile passes strict cross-language conformance; 1.0–1.3 remain compatible and omit script fields |
| `S5-04` | `ScriptSemanticAdapter` and bounded script journal | Full snapshot, exact deltas, diagnostics, invalidation, gap recovery, and GDScript-disabled build behavior stay inside the 2 ms editor main-thread budget |
| `S5-05` | Rust script wire client and normalizer | Strict DTO validation, canonical identities/ranges, relations, confidence, diagnostics, and graph digest match the oracle |
| `S5-06` | Logical schema 1.3 and `segment-v3` | `segment-v2` resource/scene results remain unchanged; script shards recover atomically after cancellation, crash, or corruption |
| `S5-07` | Semantic coordinator and cross-domain composition | Resource, scene, and script freshness remain independent; inheritance, calls, loads, and attachments activate atomically |
| `S5-08` | Two symbol MCP tools | Closed schemas, deterministic lookup/paging, cursor isolation, partial results, limits, errors, and redaction pass locally |
| `S5-09` | macOS and Windows live gates | Both hosts pass the same source freeze, canonical mutation/recovery phases, semantic digest, accuracy, and SLO gates |
| `S5-10` | Deterministic final aggregate | `S5-AC-01`–`S5-AC-12` pass; raw evidence hashes, docs, and aggregate agree byte-for-byte |

`S5-01` is frozen by
[`tests/codex/evidence/sprint-5-stage-1-contracts.json`](../../tests/codex/evidence/sprint-5-stage-1-contracts.json).
The fail-closed producer binds the exact source scope, contract/schema hashes,
toolchain and Godot artifacts; reproduces identity/range/confidence vectors in
Python, Rust and Godot C++; records the D-07 source/runtime spike; and proves
the bridge test surface builds and passes with the GDScript module disabled.
This is a contract gate, not macOS/Windows Sprint 5 acceptance evidence.

`S5-02` must keep its expected graph independent of Bridge and Rust output. The
fixture includes at least: class names and path-only scripts; inner classes;
typed/untyped methods and properties; constants, enums, and signals; base and
derived scripts with overrides; parameters and locals; exact calls; dynamic
calls; literal and dynamic load/preload; string and shorthand node paths;
parse/analyzer errors in one file; cyclic or missing dependencies; `.cs`
discovery; and scenes whose script attachment changes.

`S5-02` is frozen by
[`tests/codex/evidence/sprint-5-stage-2-oracle.json`](../../tests/codex/evidence/sprint-5-stage-2-oracle.json).
Its hand-authored truth set contains 23 fixture files, eight saved documents,
48 declarations, 14 exact/dynamic relations, 65 UTF-8 content-bound ranges,
four isolated diagnostics, and three attachment states. Strict Python/Rust
validators reproduce the graph without importing Bridge, sidecar, index, or
MCP implementation code; Godot C++ independently parses the six valid
GDScript documents and the intentionally broken document. The ten later live
phases are named here, but no live ingestion or cross-host acceptance is claimed.

`S5-06` adds independently content-addressed script-document, symbol, relation,
reference, diagnostic, and lookup shards. Migration validates the active
`segment-v2` generation, reuses its resource and scene shards, creates an empty
non-current script domain, and activates only through the durable commit marker.
Failure leaves the prior generation readable and cleanup removes incomplete
staging data before an idempotent retry.

## 5. Required data flow

1. `EditorFileSystem` reports a saved script addition, change, move, or removal.
2. `ScriptSemanticAdapter` pins the exact saved content hash and obtains semantic
   observations from the selected D-07 analyzer path.
3. The adapter emits detached bounded observations; Godot parser nodes, objects,
   source text, and editor pointers never cross the bridge boundary.
4. Bridge RPC 1.4 transfers a complete script snapshot or one contiguous delta.
5. The sidecar validates ranges, identities, targets, confidence, and limits,
   then normalizes the script graph against one pinned resource/scene generation.
6. The single semantic-index writer durably activates all affected script shards
   and cross-domain lookup shards together.
7. Symbol tools pin exactly one current compatible generation per request.

A saved script change makes the script domain non-current until its delta
commits. Current resource and scene queries remain usable. A scene attachment
change can make the composed attachment lookup non-current without discarding
unaffected script declarations. A journal gap, incompatible analyzer profile,
range/hash mismatch, corrupt generation, or failed composition forces a full
script resnapshot; stale script facts are never served as current.

## 6. Public interfaces

### 6.1 Bridge RPC 1.4

The planned compatible minor adds capabilities
`script.gdscript_semantics`, `script.incremental_index`,
`script.diagnostics`, and `script.csharp_discovery`; methods
`script.snapshot.get` and `script.delta.get`; domain `script_graph`; revision
`script_graph_revision`; and notifications `script_graph_changed` and
`script_journal_gap`.

The script projection contains bounded source-document coordinates,
declarations, semantic relations, references, literal resource targets,
diagnostics, adapter capability/status, and per-document completeness. It does
not contain raw source bytes or absolute paths. Negotiated sessions below 1.4
do not advertise, accept, or emit script-domain fields and retain their existing
resource/scene behavior.

### 6.2 MCP tools

`godot_search_symbols` accepts a non-empty query, `match` (`exact` or `prefix`),
optional language/kind/script filters, optional `limit` (default 50, range
1–200), and optional cursor. It returns deterministic declaration matches only;
locals and parameters are excluded unless the future contract adds an explicit
bounded selector.

`godot_inspect_symbol` accepts exactly one selector: opaque `symbol_id`, or a
script selector plus canonical qualified name. It returns declaration/signature,
type and visibility facts, inheritance/override relations, outgoing exact and
dynamic relations, scene attachments, diagnostics, and source evidence. It does
not perform the general reverse usage aggregation reserved for Sprint 6.

Both tools return the common project/generation/revision/freshness/evidence
envelope with `script_graph_revision`. Cursors are HMAC-authenticated, expire
after at most five minutes, and bind project, tool, normalized selector and
filters, page size, generation, index revision, resource revision, scene graph
revision when used, script graph revision, and offset.

## 7. Acceptance criteria

| ID | Requirement |
|---|---|
| `S5-AC-01` | Named symbol identity survives body-only edits and line movement; rename/owner changes and content-scoped symbols follow the frozen identity vectors |
| `S5-AC-02` | Classes, inner classes, methods/functions, properties, constants, enums, signals, parameters, and supported locals match the independent oracle with exact content-bound ranges |
| `S5-AC-03` | Script inheritance and method overrides resolve to the correct base declaration; unresolved/cyclic bases are deterministic partial diagnostics |
| `S5-AC-04` | Literal preload/load paths and UIDs resolve through the resource graph; dynamic targets are retained without an `exact` target |
| `S5-AC-05` | Saved scene nodes link to the correct attached script/class, and attachment replacement/removal atomically updates the composed relations |
| `S5-AC-06` | Every analyzer-resolvable reference/call in the frozen truth set has the correct target and range, with zero false `exact` results |
| `S5-AC-07` | Dynamic dispatch, dynamic load, and string `NodePath` cases never receive false `exact`; confidence and evidence survive persistence unchanged |
| `S5-AC-08` | A GDScript parse/analyzer error is a bounded current diagnostic and does not stop other documents; GDScript-disabled/C# scripts, attachments, and adapter availability are reported without false semantic claims |
| `S5-AC-09` | Full/incremental ingestion, rename/delete, dependent invalidation, reconnect, journal gap, cancellation, and corruption never publish a partial script generation |
| `S5-AC-10` | `segment-v2` migrates to `segment-v3` without changing resource or scene query results; migration failure preserves the prior generation |
| `S5-AC-11` | Both MCP tools pass exact registry, closed schema, selector/filter, deterministic pagination, cursor, partial-result, error, limit, and redaction tests |
| `S5-AC-12` | macOS and Windows normalized script graph digests match on one source freeze and every Sprint 5 accuracy/SLO gate passes |

## 8. Accuracy, performance, and evidence gates

- zero false `exact` references, calls, overrides, loads, or attachments in the
  independent oracle;
- 100% recall for the reference kinds explicitly marked analyzer-resolvable in
  the frozen Sprint 5 oracle; dynamic cases are excluded from that denominator
  and checked separately;
- cached symbol query p95 is at most 300 ms;
- saved script change to current query visibility p95 is at most 2 seconds;
- status/ping during bulk script indexing p95 is at most 200 ms;
- the bridge records zero main-thread samples above 2 ms;
- queues, snapshots, journals, and result pages obey the existing protocol hard
  limits or stricter script-specific limits frozen in `SCRIPT-001`/schema;
- no token, absolute project path, imported path, raw source, raw DTO, or source
  excerpt appears in ordinary logs or evidence metadata;
- macOS and Windows evidence is produced locally from one clean source freeze
  and installed only after receipt, schema, artifact, source, fixture, range,
  semantic-digest, cleanup, and redaction validation;
- qualifying macOS and Windows artifacts use the same language-adapter feature
  profile; the canonical C# case is discovery-only and requires no .NET runtime;
- the canonical platform phases are `base`, `body_edit`, `line_shift`,
  `symbol_rename`, `override_change`, `literal_dependency_change`,
  `attachment_change`, `parse_error`, `csharp_discovery`, and `journal_gap`;
- every phase records raw latency samples, analyzer mode/version, Bridge
  main-thread telemetry, normalized semantic digest, assertions, revisions, and
  cleanup status;
- final evidence records `remote_ci: not_run`; Linux is not an acceptance host.

The macOS run is the first qualifying live gate. Once it passes, its clean
commit becomes the source freeze. Windows checks out that exact commit and runs
the identical fixture phases. The aggregate fails closed unless both reports
bind the same source/fixture/oracle coordinates and all phase semantic digests
match exactly.

## 9. Schedule and dependencies

| Working day | Planned gate | Dependency |
|---|---|---|
| 1–2 | `S5-01` contract, D-07 spike, vectors; `S5-02` fixture/oracle freeze | Sprint 4 accepted baseline |
| 3 | `S5-03` Bridge RPC 1.4 schemas and conformance | `S5-01` frozen projection |
| 4 | `S5-04` C++ adapter, journal, invalidation, module tests | `S5-02/03` |
| 5 | `S5-05` Rust wire validation and normalization | `S5-03/04` |
| 6 | `S5-06` `segment-v3`, migration, recovery | `S5-05` canonical records |
| 7 | `S5-07` coordinator and resource/scene/script composition | `S5-06` |
| 8 | `S5-08` MCP tools and contract/security tests | `S5-07` current queries |
| 9 | `S5-09` macOS live gate, SLO, source freeze | All implementation gates |
| 10 | Windows live gate, deterministic aggregate, `S5-10` docs | Exact macOS source freeze |

No stage may consume a later stage as an unstated prerequisite. D-07, identity,
range, confidence, fixture, and oracle decisions freeze before wire/storage code.
A failed gate is repaired and rerun before the next dependent gate begins; the
calendar does not override acceptance.

## 10. Commit boundaries

1. Sprint plan, `SCRIPT-001`, D-07 decision, identity/range/confidence vectors.
2. Independent GDScript/C#/scene fixture and golden oracle.
3. Bridge RPC 1.4 schemas, negotiation, and C++/Rust conformance.
4. C++ `ScriptSemanticAdapter`, projection, bounded journal, and telemetry.
5. Rust script wire client, canonical normalizer, and semantic digest.
6. Logical schema 1.3, `segment-v3`, migration, and fault recovery.
7. Coordinator, independent freshness, attachments, and cross-domain relations.
8. `godot_search_symbols`, `godot_inspect_symbol`, cursors, errors, and redaction.
9. macOS runner, live evidence, SLO, and source freeze.
10. Windows evidence, deterministic final aggregate, and completion documentation.

Each commit must be independently reviewable and leave existing Sprint 1–4
tests green. Generated binaries, temporary projects, caches, tokens, absolute
paths, and unredacted traces are never committed. Evidence-only closing commits
must not change the frozen implementation or fixture source scopes.

## 11. Definition of Done

Sprint 5 is complete only when:

- all `S5-01`–`S5-10` exit conditions are satisfied;
- D-07 and the normative `SCRIPT-001` contract are committed;
- Bridge RPC 1.0–1.3 compatibility and Sprint 3/4 query parity remain proven;
- the production index is `segment-v3` with tested `segment-v2` migration and
  recovery;
- the exact MCP registry contains the existing seven tools plus the two Sprint 5
  tools, and no premature Sprint 6 find-usages tool;
- all `S5-AC-01`–`S5-AC-12` cells are true;
- macOS and Windows qualifying reports share one clean source freeze and every
  normalized phase digest;
- the deterministic aggregate, raw report hashes, contract versions, SLO data,
  redaction/cleanup assertions, and documentation agree;
- Linux and remote CI remain explicitly `not_run`, not implied successes.

The next milestone after this Definition of Done is Sprint 6: find usages,
evidence aggregation, and Codex context shaping.
