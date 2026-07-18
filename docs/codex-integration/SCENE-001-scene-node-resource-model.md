# SCENE-001 — scene, node, and built-in resource model

**Status:** D-06 contract frozen for Sprint 4 implementation

**Decision:** `D-06` — persistent node and built-in subresource identity

**Parent:** [SPRINT-4-PLAN.md](SPRINT-4-PLAN.md)

## 1. Authority and non-authority

Godot is authoritative for structural scene semantics:

| Fact | Authority |
|---|---|
| Scene resource identity and current path | `ResourceUID` plus the Sprint 3 resource index |
| Saved nodes, parents, owners, unique IDs, properties, groups, instances, and connections | `PackedScene::get_state()` / `SceneState` |
| Base scene | `SceneState::get_base_scene_state()` and the corresponding `PackedScene` resource |
| Built-in resource scene-unique ID | `Resource::get_scene_unique_id()` |
| Attached script and exported property metadata | Serialized `SceneState` property plus Godot `Script` property metadata |
| Animation tracks | Godot `AnimationLibrary`/`Animation` resources and track APIs |
| Main scene, Autoload, InputMap, and semantic layer names | `ProjectSettings` and `InputMap` |

The sidecar may normalize, join, compose, index, and query those observations. It must
not raw-parse `.tscn`, infer engine defaults that `SceneState` did not serialize, resolve
script callables without a language adapter, or claim that static node definitions are
live/runtime objects.

## 2. D-06 identity decision

All public IDs are opaque, domain-separated SHA-256 identifiers encoded as unpadded
base64url. The producer and golden vectors reproduce them exactly; clients do not parse
them.

### 2.1 Scene definition

- With a valid scene ResourceUID, the canonical input is that UID and identity scope is
  `persistent`.
- Without a UID, the canonical input is normalized comparison path plus the owning
  resource content generation; scope is `content_revision` and the result carries
  `weak_scene_identity`.
- A UID-preserving file rename does not change the scene ID.

### 2.2 Node definition

- When `SceneState::get_node_unique_id()` returns a positive assigned ID, the canonical
  input is the defining scene entity ID plus the decimal unique scene node ID; scope is
  `persistent`.
- The key is local to the defining scene. Equal numeric node IDs in different scenes are
  unrelated.
- Rename or reparent does not change a saved node definition ID.
- Godot duplication/packing must assign a distinct unique ID; a duplicate ID in one
  scene is rejected with `duplicate_scene_node_id`.
- Without a usable unique ID, the fallback input is scene ID, canonical relative
  `NodePath`, and scene content generation; scope is `content_revision` and the result
  must carry `weak_node_identity`.

### 2.3 Node occurrence

A node definition may occur more than once through instances. The occurrence input is
the root scene entity ID, the ordered chain of persistent instance-root node-definition
IDs, and the terminal node-definition ID. It is persistent only when every component is
persistent. Otherwise it includes the root scene content generation and is
`content_revision` scoped.

An occurrence is a static composed projection. It is never an editor-session `ObjectID`
or runtime object identity.

### 2.4 Built-in subresource

- With a valid non-duplicated `Resource::get_scene_unique_id()`, the canonical input is
  owner resource entity ID plus scene-unique ID; scope is `persistent`.
- Reordering text sections does not change this identity.
- Missing, invalid, or duplicated IDs use owner entity ID, owner content generation,
  canonical semantic ownership paths, resource type, and occurrence ordinal; scope is
  `content_revision` and the result carries `weak_subresource_identity`.
- Equal serialized values never merge two distinct subresources.

## 3. Structural records

The normalized index stores:

- `SceneDefinition`: scene entity, source resource, base scene, content generation, and
  project-context role;
- `NodeDefinition`: defining scene, unique ID/scope, canonical local path, parent,
  owner, Godot type, index, editable/internal flags, and attached script;
- `SceneInstance`: instance-root definition, referenced scene, placeholder/editable
  state, and declaration scope;
- `NodeOccurrence`: root scene, definition, effective path, parent/owner occurrence, and
  ordered instance chain;
- `PropertyFact`: subject, name, projected value/type, declaring scene/node, origin,
  optional overridden fact, and source revision;
- `SubresourceEntity`, `SignalConnection`, `GroupMembership`, `AnimationTrackReference`,
  `ProjectContextFact`, and deterministic diagnostics.

Every record includes authority, identity scope where applicable, scene graph revision,
resource/content revision coordinates, and a deterministic semantic key. Physical shard
names or offsets never leave the store boundary.

Here `owned` means that the node definition is serialized under the defining scene's
ownership root. `editable` and `internal` on a composed occurrence are query-scene
facts: descendants of an editable instance are editable; descendants hidden behind a
non-editable instance boundary are internal to that composed projection. These flags
do not claim that a runtime-only child exists.

## 4. Composition and provenance

Composition is deterministic and cycle-safe:

1. start with the oldest base scene;
2. apply each inherited scene in order;
3. expand instances by occurrence chain;
4. apply editable-instance and instance overrides at their declared target;
5. emit effective facts while retaining the declaring fact and override relation.

Property origin is exactly one of:

- `local` — declared on a node owned by the queried scene;
- `inherited` — effective value comes from a base scene without a later override;
- `instance_override` — the queried scene overrides a node from an instantiated scene.

An unresolved override target produces `broken_override_target`; it is not rebound by
name or approximate path. Cycles in inheritance or instancing produce a bounded
`scene_composition_cycle` diagnostic and make the affected scene invalid for current
queries.

## 5. Paths, resources, signals, groups, and animations

- Node selectors use relative node-only `NodePath`; absolute paths and subnames are
  rejected at the MCP boundary. `.` selects the root.
- `owner` and parent are resolved to node entities in the same composed occurrence.
- Attached scripts and external resources resolve through the Sprint 3 resource index;
  missing targets remain partial results with diagnostics.
- Signal records preserve emitter, signal, receiver, method, flags, binds, unbinds, and
  declaration scope. Sprint 4 does not assert that the method exists in source code.
- Group records preserve group name, member, and declaration scope.
- Animation paths are resolved relative to the owning mixer/player root. Resolution is
  `resolved`, `broken`, or `unresolved`; dynamic/script-computed paths are not invented.

## 6. Project context allowlist

Sprint 4 captures only:

- `application/run/main_scene`;
- all `autoload/*` entries, including singleton enabled state and resource target;
- InputMap actions, deadzones, and bounded projected input events;
- `layer_names/2d_physics/*`, `layer_names/2d_render/*`,
  `layer_names/3d_physics/*`, `layer_names/3d_render/*`, and
  `layer_names/navigation/*`.

Other ProjectSettings are out of scope and must not leak into a scene query.

## 7. Revisions, persistence, and recovery

`scene_graph_revision` is a project-wide monotonic bridge-session coordinate independent
from the live `scene_revisions` map. Each scene observation also carries the source
`resource_revision` and content generation.

Logical schema 1.2 and `segment-v2` add scene-domain shards to the existing store. One
writer owns all activation. Resource and scene freshness are tracked separately. A scene
file resource update marks the scene domain non-current until a matching scene commit
activates; resource queries may remain current.

Cancellation, crash, corrupt scene shards, invalid composition, or migration failure
cannot replace the previous valid generation. A gap or incompatible checkpoint requires
a full scene snapshot. The previous graph may remain retained for recovery but is never
served with `freshness: current`.

## 8. Query contract

Both scene tools pin one immutable generation. Results always include project ID,
generation ID, index revision, resource revision, scene graph revision, freshness,
status, applied limits, entities, facts, evidence, diagnostics, truncation, and partial
reasons.

The result is deterministic for a selector, generation, limit, and cursor. Pagination
never crosses generations. Missing resources, broken paths, and unresolved script
callables are partial successes; unavailable or stale indexes are structured tool
errors.

## 9. Security and limits

- Godot/editor objects are touched only in bounded main-thread work;
- scene loading uses Godot resource APIs and never an untrusted sidecar parser;
- detached data alone may be hashed, encoded, or written by workers;
- values reuse the bounded/redacted Variant projection contract;
- absolute filesystem paths, cache paths, token material, raw imported payloads, object
  addresses, and unbounded values are forbidden in results and ordinary logs;
- snapshot/delta queues, scene/node/property/relation counts, nesting, instance depth,
  variant depth, encoded bytes, result pages, and cursor lifetime have hard limits;
- overflow invalidates the scene journal and forces a full snapshot.

## 10. Required proof

D-06 is accepted only when Python, Rust, and live Godot tests reproduce the same vectors
and prove rename, reparent, save/reopen, duplication, nested instance, and subresource
reorder behavior. The full Sprint gate additionally requires exact macOS/Windows graph
digest parity and every `S4-AC-01`–`S4-AC-12` criterion from the parent plan.
