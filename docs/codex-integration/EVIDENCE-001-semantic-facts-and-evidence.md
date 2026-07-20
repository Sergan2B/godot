# EVIDENCE-001 — semantic facts and evidence

**Status:** Sprint 6 contract freeze candidate

**Version:** 1.0

## 1. Boundary

Evidence aggregation is a storage-neutral projection over one immutable index
generation. Persisted resource, scene, and script records remain authoritative;
the aggregator does not create a second source of truth or a new physical
format.

## 2. Canonical records

`SemanticFact` contains:

- deterministic `fact_id`;
- source entity ID and `source_kind`;
- canonical predicate;
- target entity ID or bounded literal;
- domain and the complete resource/scene/script revision vector;
- aggregate confidence and freshness;
- one or more sorted `EvidenceRecord` values.

`EvidenceRecord` contains:

- deterministic `evidence_id`;
- producer source and authority;
- optional normalized `res://` path, NodePath, property, or content-bound source
  range;
- confidence and freshness owned by that observation;
- index/resource/scene/script revisions and optional content SHA-256.

`ConflictDiagnostic` identifies a single-valued semantic slot that has multiple
authoritative current values. It lists every retained `fact_id`; it never chooses
an arbitrary winner.

## 3. Vocabulary

Canonical predicates are `references`, `instantiates`, `inherits`, `overrides`,
`attaches_script`, `declares_symbol`, `references_symbol`, `connects_signal`,
`belongs_to_group`, `preloads`, `loads`, and `calls`.

Entity kinds are `project`, `resource`, `scene`, `node`, `script`, `symbol`, and
`signal`. Confidence values are `exact`, `probable`, `dynamic`, and the reserved
future value `runtime_confirmed`. Freshness is `current` for Sprint 6 results;
unavailable/non-current domains are reported as partial reasons, never as stale
facts disguised as current.

## 4. Identity, aggregation, and conflicts

`fact_id` is domain-separated SHA-256 over canonical JSON containing source ID,
predicate, target/literal, domain, and the full pinned revision vector.
`evidence_id` is domain-separated SHA-256 over the fact identity plus producer,
authority, location/range, content hash, confidence, and revision coordinates.

Facts deduplicate only when their complete fact identity matches. Evidence then
deduplicates by `evidence_id` and sorts canonically. Text similarity is never an
identity rule. Different source/target IDs or revisions remain distinct.

An exact authoritative observation may support an exact fact even if a weaker
observation also supports the same identity, but weaker evidence is preserved.
No collection of probable/dynamic evidence can create exact confidence.

Only predicates declared single-valued for a specific semantic slot participate
in conflict detection. Multi-target references, calls, groups, and signal
connections are not conflicts merely because more than one target exists.

## 5. Safety and limits

- Evidence never contains absolute project/import paths, raw source, secrets,
  session tokens, Bridge endpoints, or unbounded Variant values.
- Every usage result has evidence or is rejected during projection.
- One request pins one generation; cross-generation aggregation is invalid.
- Source ranges remain bound to exact normalized path and content SHA-256.
- Results obey the same project isolation, pagination, cancellation, and
  structured-error rules as the existing MCP index tools.
