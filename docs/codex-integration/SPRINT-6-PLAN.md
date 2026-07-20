# Sprint 6 plan — find usages, evidence, and Codex context

**Status:** In progress — contract and oracle freeze first

**Milestone:** Semantic Alpha / M1

**Baseline:** `437797bf6f96b7b10b9e4f2ea0a13aa76c5d7436`

**Normative contracts:** [EVIDENCE-001](EVIDENCE-001-semantic-facts-and-evidence.md),
[CONTEXT-001](CONTEXT-001-model-facing-context.md),
[MCP-001](MCP-001-project-scoped-read-tools.md), and
[INDEX-001](INDEX-001-semantic-index-storage-and-migrations.md)

## 1. Outcome and fixed decisions

Sprint 6 joins the persisted resource, scene, and saved-script domains into one
revision-pinned evidence view. It adds a tenth read-only MCP tool,
`godot_find_usages`, plus stable project and scene summary resources for Codex.

- Bridge RPC remains `1.4`; MCP remains `2025-11-25`.
- Logical schema remains `1.3`; physical storage remains `segment-v3`.
- Reverse query indexes are derived in memory and keyed by immutable
  `generation_id`; no new persisted shards or migration are introduced.
- The blocking gate is deterministic and model-free on macOS arm64.
- One local Codex CLI smoke is advisory and cannot change the deterministic
  result.
- Windows, Linux, and remote CI are `not_run` Sprint 6 coordinates.

Live unsaved overlays, runtime evidence, writes/transactions, and deep C#
semantics remain outside Sprint 6.

## 2. Gates

| Gate | Deliverable | Exit condition |
|---|---|---|
| `S6-01` | `EVIDENCE-001`, `CONTEXT-001`, this plan | Public facts, evidence, selectors, budgets, errors, and scope are frozen |
| `S6-02` | Independent semantic-context fixture and oracle | Oracle validates without importing index/query/MCP implementation code |
| `S6-03` | One pinned semantic read view | No request can combine torn generations; unavailable domains remain explicit |
| `S6-04` | Evidence aggregation and derived indexes | Duplicate facts merge provenance; conflicting facts remain visible |
| `S6-05` | Storage-neutral find-usages query | Canonical filters, ordering, limits, paging, and partial rules pass unit tests |
| `S6-06` | `godot_find_usages` MCP tool | Exact ten-tool registry, closed schema, signed cursor, errors, and redaction pass |
| `S6-07` | Project and scene summary builders | Deterministic summaries stay within 4096/2048 conservative token bounds |
| `S6-08` | MCP resources and Godot `AGENTS.md` template | Stable resources/read and semantic-first instructions pass contract tests |
| `S6-09` | macOS benchmark and live evidence | Accuracy, evidence, dedup, paging, budget, latency, cleanup, and redaction pass |
| `S6-10` | Deterministic closeout | Raw report, acceptance artifact, hashes, versions, and docs agree |

## 3. Query and evidence rules

The query resolves exactly one target from a canonical entity ID, resource,
scene, scene/node path, script/qualified symbol, or scene/emitter/signal
selector. Optional filters constrain source entity kinds, confidence, and
project/scene/script scope. The default page is 50; pages accept 1–200 records;
the total bounded result window is 250,000.

Results sort by source kind, source identity, predicate, then fact identity.
Signed cursors bind the normalized target, every filter, limit, project,
generation, and resource/scene/script revision. Cross-tool, cross-filter,
cross-scope, expired, modified, or stale cursors fail closed.

Every returned usage has at least one evidence record. Duplicate provenance is
aggregated without losing sources. Different entity identities or revision
vectors are not deduplicated. A dynamic or probable observation cannot become
exact merely because another producer emitted the same text.

## 4. Context resources

- `godot://project/summary` is a listed fixed resource with a budget of 4096.
- `godot://scene/{scene_id}/summary` is a resource template with a budget of
  2048.
- Both return canonical `application/json` and report truncation and omitted
  counts.
- `utf8_byte_upper_bound_v1` limits serialized UTF-8 bytes to the token budget,
  avoiding a model-specific tokenizer while guaranteeing a conservative bound.
- Resource subscriptions and list-change notifications are not enabled.

## 5. Acceptance

The independent oracle includes resource UID rename, scene inheritance and
instances, node references, signals, groups, script symbols/calls/loads,
duplicate provenance, partial domains, conflicts, paging, and budget pressure.

Sprint 6 passes only when exact recall matches the frozen truth set with zero
false exact, every usage has evidence, duplicates and conflicts follow
`EVIDENCE-001`, summaries obey budgets, cursor/security/redaction tests pass,
cached queries and summary reads have p95 at most 300 ms, and control ping during
the benchmark has p95 at most 200 ms.

The advisory Codex CLI smoke asks three fact-free questions: symbol usages,
scene explanation, and resource-rename impact. Its version, prompt, tool trace,
and evidence citations are recorded when available; it is never substituted for
the deterministic gate.

## 6. Commit boundaries

1. Contracts and Sprint plan.
2. Independent fixture, schemas, oracle, and validators.
3. Semantic view, aggregation, deduplication, and conflicts.
4. Storage-neutral query and tenth MCP tool.
5. Bounded summaries, resources, and guidance template.
6. macOS acceptance evidence and completion documentation.

Each commit is independently reviewable. Closing evidence commits do not modify
the frozen implementation or oracle source scopes.
