# SCRIPT-001 — GDScript and C# semantic adapters

**Status:** D-07 accepted; `S5-01` contract, independent `S5-02` oracle, Bridge
RPC 1.4 `S5-03`, bounded editor adapter/journal `S5-04`, and strict Rust wire
normalization `S5-05` are locally verified on macOS arm64; `S5-06`–`S5-10` and
two-host acceptance remain pending

**Decision:** `D-07` — use a bridge-owned saved-content cache over Godot's
GDScript parser/analyzer; do not depend on the active LSP peer cache

**Parent:** [SPRINT-5-PLAN.md](SPRINT-5-PLAN.md)

**Contract evidence:**
[`tests/codex/evidence/sprint-5-stage-1-contracts.json`](../../tests/codex/evidence/sprint-5-stage-1-contracts.json)

**Independent oracle evidence:**
[`tests/codex/evidence/sprint-5-stage-2-oracle.json`](../../tests/codex/evidence/sprint-5-stage-2-oracle.json)

**Bridge RPC 1.4 evidence:**
[`tests/codex/evidence/sprint-5-stage-3-bridge.json`](../../tests/codex/evidence/sprint-5-stage-3-bridge.json)

## 1. Authority and non-authority

Godot is authoritative for saved-script semantics:

| Fact | Authority |
|---|---|
| Script resource identity and current path | `ResourceUID` plus the Sprint 3 resource index |
| GDScript syntax, declarations, ranges, and diagnostics | `GDScriptParser` and the parser diagnostics it produces |
| Unambiguous GDScript types, inheritance, overrides, and reference targets | `GDScriptAnalyzer` |
| Literal preload/load resource target | Parser/analyzer observation joined to the Sprint 3 resource index |
| Saved scene attachment | Sprint 4 `SceneState` attached-script fact joined to the script graph |
| Optional workspace projection | `ExtendGDScriptParser` over the same parser/analyzer version at the exact saved content hash |
| C# language-service availability and diagnostics | The registered C# language adapter, when present |

The sidecar may validate, normalize, join, persist, and query these observations.
It must not parse GDScript independently, treat text matching as a semantic
reference, infer a dynamic call target, consume a dirty LSP document as saved
state, or claim C# semantics when the corresponding adapter is unavailable.

Raw source bytes are an input to the editor-side language adapter and to the
content hash only. They are not Bridge DTO fields, index records, ordinary logs,
or evidence metadata.

## 2. D-07 analyzer integration decision

### 2.1 Rejected: active LSP peer cache as the required source

The current fork's `GDScriptLanguageProtocol` cache is unsuitable as the
correctness boundary:

- `get_parse_result()` requires an active LSP client and otherwise returns no
  parser;
- an LSP-managed document is parsed from `document->text`, which may be an
  unsaved editor/IDE buffer;
- a disk-only parse is marked stale because the LSP cannot invalidate it
  reliably and is removed after the request;
- cached entries are keyed by path and expose neither the required SHA-256
  saved-content coordinate nor a stable cross-client lifetime;
- LSP connection state and client capabilities are unrelated to whether the
  Godot × Codex static index must be correct.

These are correctness failures, not performance trade-offs. An active LSP cache
therefore cannot be a required Sprint 5 dependency.

### 2.2 Selected: bridge-owned exact-content projection cache

The bridge reads one saved file revision, computes its SHA-256, parses that exact
UTF-8 content with the built-in parser/analyzer, projects detached bounded
records, and caches the projection by:

```text
(script resource ID, language, content SHA-256, analyzer profile)
```

`ExtendGDScriptParser` may be reused as an internal projection helper because it
invokes `GDScriptParser`, then `GDScriptAnalyzer`, and already exposes document
symbols and diagnostics. Its LSP JSON objects are not the wire contract. The
bridge owns cache lifetime, source-hash validation, invalidation, limits, and DTO
shaping.

An LSP-produced projection may become an optimization only if a later
implementation proves all of the following before reuse:

- its exact content SHA-256 equals the saved file hash;
- its analyzer profile equals the negotiated script adapter profile;
- it contains no dirty-buffer overlay;
- ownership/lifetime remains valid for the complete projection copy;
- the normalized result digest equals the bridge-owned path.

Failure of any condition falls back to the bridge-owned path. The integration
works when the language server setting is disabled and when no LSP client has
ever connected.

### 2.3 GDScript-disabled behavior

The Codex bridge must still compile when the GDScript module is disabled. It
does not advertise `script.gdscript_semantics`; script-domain requests return
the structured `capability_unavailable` error. Existing editor, resource, and
scene capabilities remain unchanged.

## 3. Canonical identity

All public IDs are opaque domain-separated SHA-256 values encoded as unpadded
base64url. Inputs are UTF-8 strings separated by one zero byte. The domain is
also followed by one zero byte. Clients store and return IDs but never parse
them.

### 3.1 Named symbol

A named script, class, or class member uses:

```text
domain = "godot-codex/script-symbol/named/v1"
input  = script_resource_id || 0x00 || language || 0x00 || kind || 0x00 || qualified_key
prefix = "godot:script-symbol:named:v1:"
scope  = persistent
```

The qualified key uses the exact case-sensitive analyzer spelling and an
explicit owner/kind path, for example:

```text
script
class:Player
class:Player/property:health
class:Player/method:take_damage
class:Outer/class:Inner/signal:finished
```

GDScript does not normalize identifier spelling before lookup, so the identity
algorithm must not NFC-fold, case-fold, or confusable-fold a qualified key.
Body-only edits and line movement preserve a named ID. Rename or owner change
creates a different ID. An invalid duplicate declaration produces diagnostics;
it does not mint two ambiguous persistent symbols.

### 3.2 Content-revision symbol

Parameters, locals, lambdas, anonymous symbols, and any declaration without a
proven named key use:

```text
domain = "godot-codex/script-symbol/content-revision/v1"
input  = script_resource_id || 0x00 || language || 0x00 || content_sha256
         || 0x00 || owner_qualified_key || 0x00 || kind
         || 0x00 || decimal_start_byte || 0x00 || decimal_end_byte
prefix = "godot:script-symbol:content-revision:v1:"
scope  = content_revision
```

Line movement or any content change may change this identity. Such a symbol is
never presented as stable across source revisions.

### 3.3 Diagnostic

A parser/analyzer diagnostic uses:

```text
domain = "godot-codex/script-diagnostic/content-revision/v1"
input  = script_resource_id || 0x00 || language || 0x00 || content_sha256
         || 0x00 || severity || 0x00 || stable_code
         || 0x00 || decimal_start_byte || 0x00 || decimal_end_byte
         || 0x00 || normalized_safe_message
prefix = "godot:script-diagnostic:v1:"
scope  = content_revision
```

Messages are normalized only for stable whitespace and redaction. The stable
code, not localized prose, is the primary machine-readable classification.

## 4. Source coordinates

The canonical location is bound to:

- normalized project-relative `res://` path;
- exact on-disk UTF-8 source SHA-256;
- zero-based, half-open UTF-8 byte span `[start_byte, end_byte)`.

Human-facing coordinates are derived as one-based line and one-based Unicode
scalar column. Tabs count as one scalar, not a visual tab width. Line endings
remain part of the exact source bytes; CRLF is not silently rewritten before
hashing or byte-offset calculation. The line after either LF or CRLF begins at
column one. A UTF-8 BOM, when accepted by Godot, remains part of the byte
coordinate space.

The adapter verifies file size/mtime or an equivalent revision coordinate before
and after reading. A concurrent change discards the observation and requeues the
document. No location may be interpreted against a different content hash.

## 5. Normalized records

The script domain stores:

- `ScriptDocument`: resource identity, path, language, content hash, adapter
  profile, parse state, completeness, and revision;
- `ScriptSymbol`: identity/scope, kind, name, qualified key, owner, signature,
  type, visibility, modifiers, declaration range, and documentation presence;
- `ScriptRelation`: source, predicate, optional exact target, confidence,
  evidence range, and dynamic detail;
- `ScriptDiagnostic`: stable code, severity, safe message, range, authority, and
  content revision;
- `LanguageAdapterStatus`: language, availability, profile/version, and bounded
  unavailability diagnostic.

Supported Sprint 5 declaration kinds are `script`, `class`, `method`,
`function`, `property`, `constant`, `enum`, `enum_member`, `signal`,
`parameter`, `local`, and `lambda`. Parameters/locals/lambdas remain
content-revision scoped and are excluded from default project symbol search.

Supported relations are the shared vocabulary `contains`, `declares_symbol`,
`inherits`, `overrides`, `references_symbol`, `calls`, `preloads`, `loads`, and
`attaches_script`. Physical segment IDs and analyzer pointers never leave their
component boundary.

## 6. Confidence and dynamic behavior

Sprint 5 language adapters emit only `exact` and `dynamic`:

- `exact` requires one unambiguous parser/analyzer target or a structurally exact
  resource/scene join;
- `dynamic` records known syntax whose target depends on a runtime value or
  attachment context;
- `probable` is reserved for a future bounded inference contract and is not
  emitted by Sprint 5.

Literal `preload()`/`load()` can be exact only after the resource index resolves
the literal UID/path. `load(variable)`, `call(variable)`, Variant dispatch, and
string-based method names are dynamic. `$Node`, `%UniqueNode`,
`get_node("Path")`, and other string `NodePath` observations remain dynamic in
Sprint 5 because one script may be attached in multiple scene contexts and the
runtime tree may differ.

Freshness is orthogonal to confidence. An exact fact from a stale content hash
remains exact historical evidence but is not servable as current. A parse error
never revives symbols from the prior content hash as current.

## 7. Parsing, dependencies, and partial documents

The adapter analyzes one exact saved document revision and records a per-document
state:

- `complete` — parsing and required semantic resolution succeeded;
- `partial` — safe declarations/diagnostics exist but one or more dependencies
  or bodies could not be resolved;
- `invalid` — the current content has no safe semantic projection beyond
  diagnostics;
- `unavailable` — the language adapter is absent.

One invalid script does not stop other documents from advancing. References to
an invalid/missing dependency remain unresolved with diagnostics. Dependency
changes invalidate exact dependants through the analyzer dependency graph; the
writer activates the affected set atomically or not at all.

## 8. Revision, persistence, and recovery

`script_graph_revision` is a project-wide monotonic bridge-session coordinate
independent from resource and scene revisions. Each document also carries its
resource revision and source content hash.

Logical schema 1.3 and `segment-v3` add script document, symbol, relation,
diagnostic, and lookup shards. Resource, scene, and script freshness are tracked
independently inside one immutable generation. A saved script update makes only
the script domain non-current until a matching commit activates; resource/scene
queries may remain current.

Cancellation, crash, corruption, invalid normalization, or migration failure
cannot replace the previous valid generation. Journal overflow, a delta gap, or
an incompatible analyzer profile forces a full script snapshot. Retained older
facts are never relabeled current.

## 9. Query contract

`godot_search_symbols` returns deterministic named declaration matches by exact
or prefix query with bounded language/kind/script filters.
`godot_inspect_symbol` returns one declaration plus its owner, signature/type,
inheritance/override, outgoing exact/dynamic relations, scene attachments,
diagnostics, and evidence. Reverse project-wide usages remain Sprint 6 scope.

Both tools pin one immutable generation and return project ID, generation ID,
index revision, resource/scene revisions when used, script graph revision,
freshness, status, applied limits, entities, facts, evidence, diagnostics,
partial reasons, truncation, and an authenticated generation-bound cursor.

## 10. Minimal C# abstraction

Sprint 5 defines the shared `LanguageSemanticAdapter` capability/status envelope
and discovery-only C# records:

- `.cs` script resource identity/path;
- saved scene attachments;
- whether an in-process/registered language adapter is available;
- bounded diagnostics supplied by that adapter, if present.

No C# symbol/reference is marked exact without an authoritative adapter result.
The canonical Sprint 5 cross-platform profile is discovery-only and requires no
.NET runtime or bundled Roslyn process. Deep C# semantics remain later scope.

## 11. Security and limits

- parser/analyzer access follows Godot module/thread requirements and is
  instrumented inside the existing 2 ms bridge slice budget;
- only detached bounded records cross to transport/worker code;
- source documents, parser nodes, object IDs, pointers, absolute paths, tokens,
  imported payloads, and raw DTOs are absent from results and ordinary logs;
- paths, names, signatures, diagnostics, documentation metadata, relation counts,
  dependency depth, snapshot bytes, journal entries, result pages, and cursor
  lifetime have hard schema limits;
- unsafe or oversized observations invalidate the affected batch rather than
  being silently truncated into an `exact` result;
- C# or GDScript adapter absence is a capability state, not a reason to load an
  undeclared executable or network service.

## 12. Required proof

`S5-01` closes only when Python, Rust, and Godot C++ tests reproduce the committed
identity, range, and confidence vectors; strict Draft 2020-12 validation rejects
named negatives; the D-07 prototype proves valid/invalid GDScript projection
without an LSP client and records cold/warm/incremental timing plus bounded
memory; and committed evidence binds the exact source, vectors, toolchain, and
decision.

Full Sprint 5 acceptance additionally requires the independent `S5-02` oracle,
Bridge/index/MCP implementation, same-freeze macOS/Windows graph digest parity,
and every `S5-AC-01`–`S5-AC-12` criterion from the parent plan.
