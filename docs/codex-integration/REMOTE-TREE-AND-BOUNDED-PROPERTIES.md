# Remote tree and bounded properties

**Status:** Implemented Sprint 8 Bridge-core workstream

**Wire profile:** Bridge RPC `1.6`

**Parent contract:** [RUNTIME-001](RUNTIME-001-debugger-and-runtime-observation.md)

## 1. Boundary and authority

The game-side `SceneDebugger` is authoritative for the live `SceneTree`,
object existence, Inspector property metadata, and property values. The
editor-side `RuntimeDebuggerAdapter` owns runtime coordinates, opaque public
identities, correlation, snapshot assembly, and retirement.

The debugger channel may carry an internal `ObjectID`. Bridge RPC and every
consumer-facing result may carry only the session-scoped opaque identity.
MCP pagination and runtime-to-editor source joining are downstream consumers
and are not part of this contract.

## 2. Remote tree capture

`scene:codex_runtime_tree` carries a correlation ID and the fixed node/depth
limits. The game returns a flat depth-first prefix in natural child order.
Only captured children contribute to `child_count`, so the prefix is
self-consistent even when truncated.

The game applies the 10,000-node, 256-level, and 1,024-character identity
limits while walking the tree, before debugger serialization. Script types
may use a global class name or a safe `res://` script path; unsafe or absolute
script paths fall back to the native Godot class.

The editor validates record shape, parent balance, depth, counts, paths, and
limits independently. It computes a canonical projected-tree checksum. A
changed checksum publishes exactly one `tree_changed` event; an identical
resnapshot does not advance the runtime sequence.

## 3. Identity and source hints

`runtime_object_id` is base64url SHA-256 over a domain separator, the current
`runtime_session_id`, and the internal object ID. It is deterministic only
inside one runtime session and cannot be reversed to the raw ID.

Each accepted tree atomically replaces the object lookup table. Object
inspection is permitted only for an opaque ID in that table. Source hints are
limited to safe `res://` scene paths, at most 256 instance-scene paths, and a
1,024-character relative node path. They contain no editor IDs and make no
source-mapping claim.

## 4. Bounded property collection

`scene:codex_runtime_object` selects one internal object associated with the
current tree. Collection stops at 512 records before evaluating any later
getter. Script-instance property-list order is retained; language members not
present in that list are ordered derived-to-base and lexically, followed by
constants in the same stable order. Base Inspector metadata retains Godot's
property-list order.

Every returned property contains `name`, declared Variant type, usage flags,
`read_only: true`, and a projected value. A failed `Object::get` becomes a
`getter_failed` typed omission while other properties continue. No setter,
live-edit message, method call, or mutation belongs to the observation path.

## 5. Variant projection

The game and editor use one projection policy:

- depth 8, 1,000 container entries, and 16,384 characters per string;
- 64 KiB per projected value and 256 KiB per complete object result;
- deterministic dictionary key labels;
- snapshot-local `reference_id` values for shared arrays/dictionaries and
  explicit references for cycles;
- `res://` references for resources; non-project resource paths are omitted;
- runtime Objects become opaque object references only when present in the
  current tree;
- RID, Callable, Signal, pointers, native handles, packed-array payloads, and
  host absolute paths become typed omissions.

Stable omission reasons include `max_depth`, `max_items`, `max_string`,
`max_encoded_bytes`, `unsupported_handle`, `unsafe_resource_path`, and
`getter_failed`. A `max_string` result retains only its bounded prefix and
marks the typed projection as truncated.

Every clipping or omission sets `limits_applied.truncated`. The containing
object result remains valid unless its total byte budget is exhausted.

## 6. Snapshot, freshness, and retirement

`runtime.snapshot.get` transfers one immutable runtime revision cut through
`snapshot.begin`, ordered checksum-bound chunks, `snapshot.end`, and ACKs.
Limits are 16 MiB total, 512 KiB advertised chunks, a 32 MiB unacknowledged
window, and a 10-second transfer deadline. Tree and object observations have
three-second request deadlines and one in-flight request of each kind.

Session is checked before optional expected sequence. Cancellation, timeout,
correlation mismatch, and late responses cannot complete a request twice.
Disconnect, terminal lifecycle state, session replacement, reconnect
invalidation, and editor shutdown clear the tree checksum and all internal to
opaque identity maps.

Each accepted resnapshot advances an internal tree generation even when its
checksum is unchanged. The public runtime sequence advances only for a changed
checksum, while an object response started against an earlier generation fails
as `runtime_object_stale`. A structurally invalid tree never partially replaces
the last accepted tree or identity table. Observation cancellation/timeout
retires the table and publishes `snapshot_cancelled` invalidation.

Stable errors are `runtime_inactive`, `stale_runtime_session`,
`stale_runtime_state`, `runtime_disconnected`, `runtime_crashed`,
`runtime_request_timeout`, `runtime_object_not_found`,
`runtime_object_stale`, and `runtime_data_retired`.

## 7. Verification map

| Requirement | Evidence |
|---|---|
| DFS prefix, child counts, node/depth limits | `CodexS8Runtime` native tree test |
| limit before discarded getters | `CodexRTPProperties` native collector test |
| cycles, strings, containers, handles, paths | shared projector native tests |
| raw IDs, depth, writable fields rejected | Bridge 1.6 named-negative schema fixtures |
| strict consumer validation | Rust bridge-client runtime tests |
| chunks, checksums, ACKs, timeout | Bridge/Rust snapshot profiles and live fixture |
| stale IDs and terminal retirement | model-free Sprint 8 runtime fixture |

Qualifying evidence is produced from a clean, source-bound local macOS arm64
coordinate. Evidence for MCP shaping and source joining is recorded by their
separate workstreams.
