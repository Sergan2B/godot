# RUNTIME-001 — EditorDebugger lifecycle and runtime observation

**Status:** Approved Sprint 8 contract

**Contract version:** `1.0`

**Wire version:** Bridge RPC `1.6`

**Parent documents:** [PRODUCT-001](PRODUCT-001-semantic-bridge-vision-and-plan.md),
[ARCHITECTURE-001](ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md),
[PROTOCOL-001](PROTOCOL-001-bridge-rpc-v1.md), and
[MCP-001](MCP-001-project-scoped-read-tools.md)

## 1. Authority, ownership, and lifetime

`EditorRunBar`, `EditorRun`, `EditorDebuggerNode`, and the active
`ScriptEditorDebugger` session are authoritative for local game lifecycle.
The remote debugger tree, inspector observations, errors, and stack dumps are
authoritative only for the active runtime session that produced them.

The bridge observes manual editor runs and MCP-initiated runs. Exactly one
local child/debugger session is supported. Multiple child processes or active
debugger sessions produce `runtime_ambiguous`; no runtime object, control
target, or source mapping is selected.

Runtime state is memory-only. Tree, values, screenshots, raw remote IDs, and
correlation state are retired at stop, crash, disconnect expiry, project
invalidation, or editor-session change. Terminal status plus already-redacted
diagnostics/stacks may remain until the next run or editor shutdown. Nothing is
written to the persistent semantic index.

## 2. Common envelope and revisions

Every runtime result carries:

```json
{
  "schema_version": "runtime/1.0",
  "project_id": "project:sha256:...",
  "editor_session_id": "editor:...",
  "runtime_session_id": "runtime:...",
  "runtime_event_seq": 17,
  "state": "running",
  "snapshot_id": "snapshot:...",
  "coverage": [],
  "partial_reasons": [],
  "evidence": [],
  "diagnostics": [],
  "limits_applied": {}
}
```

`runtime_session_id` is 16 cryptographically random bytes encoded as 32
lowercase hexadecimal characters after `runtime:`. It is allocated for every
accepted run attempt, including attempts that subsequently fail or time out,
and is never reused. `runtime_event_seq` starts at 1 and advances on every
externally visible lifecycle transition, changed tree digest, diagnostic,
stack, successful object observation, or screenshot capture.

The Bridge revision vector contains optional `runtime_session_id` and
`runtime_event_seq`. They are absent while inactive and remain present for a
retained terminal record. Runtime coordinates are compared only together.

## 3. State machine and controls

States are `starting`, `running`, `paused`, `stopping`, `disconnected`,
`stopped`, `crashed`, `failed`, and `timed_out`. No session exists while the
runtime is `inactive`.

```text
inactive -> starting -> running <-> paused -> stopping -> stopped
                         |   |
                         +---+-> disconnected -> running | crashed | stopped
starting -> failed | timed_out
```

- A manual `play_pressed` or accepted bridge run creates `starting`.
- An MCP run never invokes scene/script save. Dirty or unsaved editor state is
  rejected with `runtime_unsaved_changes`.
- `runtime.run` while the editor already owns a game returns
  `runtime_already_active`; restart is always explicit stop followed by run.
- Pause is confirmed only by debugger `breaked`; continue is confirmed only
  by `continued`. UI button state is not evidence.
- A normal remote quit becomes `stopped`. An unexpected peer/process exit
  becomes `crashed`. Startup failure becomes `failed`.
- `disconnected` immediately retires tree and property data. A reconnect of
  the same internal child within 2 seconds retains the session ID but requires
  a full snapshot; otherwise it becomes terminal.

Stop, pause, continue, object inspection, stack retrieval, and capture require
the expected runtime session. An optional expected sequence provides optimistic
freshness. Mismatches fail with `stale_runtime_session` or
`stale_runtime_state` and return only safe current coordinates.

## 4. Runtime tree and identity

The game/editor implementation details and independent safety checks are
frozen in [Remote tree and bounded properties](REMOTE-TREE-AND-BOUNDED-PROPERTIES.md).

A runtime node contains opaque `runtime_object_id`, optional parent identity,
name, Godot or script type, canonical runtime `NodePath`, depth, child count,
visibility flags, and bounded source hints. Source hints contain only the
launched `res://` scene, instance-root `res://` paths, and relative node paths.

Object, thread, diagnostic, and stack IDs are domain-separated SHA-256
identities over the runtime session and the internal debugger identifier. Raw
Godot `ObjectID`, PID, RID, pointer, window/texture handle, endpoint, or
absolute path MUST NOT cross Bridge RPC or MCP.

Tree capture is a deterministic depth-first prefix with explicit truncation.
The game-side debugger enforces node/depth limits before serialization; the
editor adapter and Rust client validate them independently.

## 5. Bounded object inspection

Inspection accepts exactly one opaque object from the current tree snapshot.
It returns at most 512 editor-visible properties. Each record carries name,
declared Variant type, read-only status, usage metadata, projected value, and
explicit truncation/omission state.

Variant projection uses depth 8, 1000 container entries, 16384 characters per
string, 65536 encoded bytes per value, and 262144 bytes for the complete
object result. Compound cycles become snapshot-local references. Resources
become safe UID/path references. References to runtime Objects become opaque
runtime-object references. Unsupported handles and getter failures become
typed omissions and diagnostics while the containing result continues.

The game-side inspector applies property/value/total budgets before sending
the observation. No runtime setter or live-edit message is part of this
contract.

## 6. Diagnostics and stacks

The bridge retains the newest 200 runtime diagnostics, at most 16384 UTF-8
bytes per message and 262144 bytes total. Records contain an opaque diagnostic
ID, severity, source, redacted message, repeat count, runtime sequence,
project-relative source location, and optional opaque stack ID.

At most 64 stacks and 128 frames per stack are retained, with a 262144-byte
aggregate limit. Frames use one-based line/column coordinates, project-relative
script paths, and bounded function names. Non-project paths are removed.
Source text, locals, raw thread IDs, and engine pointers are forbidden.

Errors received with a callstack retain that callstack without requiring a
pause. A confirmed debugger break may publish an active pause stack. Terminal
retention never upgrades a stack's freshness to current.

## 7. D-09 runtime-to-source mapping

The sidecar, not the bridge, joins one immutable runtime snapshot with one
persistent scene generation and one applicable live-editor snapshot.

1. Resolve the launched root scene and full runtime node path to exactly one
   composed `NodeOccurrence`.
2. Validate observed instance-root scene hints and type/name; never fuzzy-match.
3. Link a live editor node only when its scene/path occurs uniquely in a
   complete snapshot at the stated editor revisions.
4. Emit `runtime_instance_of` only for a unique result, with
   `confidence: runtime-confirmed`, the runtime session/sequence, and evidence
   from debugger tree plus `SceneState`; include live-editor evidence when used.
5. Ambiguous, dynamic, truncated, dirty-incomplete, or runtime-only nodes stay
   unmapped and carry an explicit diagnostic. Runtime evidence never changes a
   static fact's confidence to `exact`.

## 8. Screenshot capture

Viewport capture is available only for the active local game viewport. One
capture may be in flight and the minimum interval is 1000 ms.

The callback path is treated as untrusted. It must resolve to a regular,
non-symlink `scr-*.png` directly inside the canonical OS temporary directory.
Input larger than 32 MiB is rejected. The bridge loads and deletes the file,
fits the image within 1280x720, re-encodes PNG, and halves dimensions until the
payload is at most 512 KiB or reaches 160x90. Failure at the minimum size is
`runtime_capture_too_large`.

Bridge RPC returns MIME type, dimensions, byte length, SHA-256, and base64url
PNG bytes. MCP converts bytes to image content and does not duplicate them in
text or structured metadata. Pixels are not retained after the tool result.

## 9. Wire methods, notifications, and errors

Bridge RPC 1.6 methods are `runtime.run`, `runtime.stop`, `runtime.pause`,
`runtime.continue`, `runtime.snapshot.get`, `runtime.object.inspect`,
`runtime.stack.get`, and `runtime.viewport.capture`.

`runtime.snapshot.get` transfers any subset of `runtime_state`, `runtime_tree`,
`runtime_diagnostics`, and `runtime_stacks` through the existing checksum and
ACK snapshot flow. A snapshot is one immutable runtime revision cut and chunks
contain only `runtime_state`, `runtime_node`, `runtime_diagnostic`, or
`runtime_stack` entities.

`runtime.event` carries session, sequence, event type, resulting state,
changed domains, and the complete negotiated revision vector.
`runtime.invalidated` reports a journal gap, overflow, session replacement,
same-session debugger reconnect, or failed recovery. A reconnect uses reason
`reconnected`, preserves the runtime session ID, and requires a full runtime
snapshot before live data can be served again. The journal is bounded to 1024
records and 4 MiB.

Stable runtime errors include `runtime_inactive`, `runtime_already_active`,
`runtime_ambiguous`, `runtime_unsaved_changes`, `runtime_start_failed`,
`runtime_start_timeout`, `stale_runtime_session`, `stale_runtime_state`,
`runtime_not_running`, `runtime_not_paused`, `runtime_disconnected`,
`runtime_crashed`, `runtime_request_timeout`, `runtime_control_timeout`,
`runtime_object_not_found`, `runtime_object_stale`, `runtime_data_retired`,
`runtime_capture_unavailable`, `runtime_capture_rate_limited`, and
`runtime_capture_too_large`.

## 10. Limits, timeout, and recovery

| Domain | Limit |
|---|---:|
| Tree nodes / depth | 10000 / 256 |
| Runtime snapshot / chunk / window | 16 MiB / 512 KiB / 32 MiB |
| Properties / Variant depth / container entries | 512 / 8 / 1000 |
| String / value / object result | 16384 chars / 64 KiB / 256 KiB |
| Diagnostics / message / total | 200 / 16 KiB / 256 KiB |
| Stacks / frames / total | 64 / 128 / 256 KiB |
| Screenshot / source / rate | 512 KiB / 32 MiB / 1 Hz |

Startup has a 10-second deadline. Pause, continue, tree, object, stack, and
capture have 3-second deadlines. Stop confirmation has 5 seconds. A complete
runtime snapshot has 10 seconds.

All operations are cancellable state machines. No main-thread code performs a
blocking wait. Late correlated responses are discarded. Crash, disconnect,
stop, cancellation, and timeout produce exactly one terminal RPC response.
Bridge lifecycle/ping work keeps control-lane priority during a hung game.

## 11. MCP projection

New read-only tools are `godot_get_runtime_tree`,
`godot_inspect_runtime_object`, `godot_get_stack_trace`, and
`godot_capture_viewport`. New control tools are `godot_run_project`,
`godot_run_current_scene`, `godot_stop_project`, `godot_pause_project`, and
`godot_continue_project`.

`godot_get_diagnostics` adds `scope: editor|runtime`, defaulting to `editor`.
`godot_get_editor_state` adds a runtime summary. The fixed
`godot://runtime/summary` resource has a 4096-byte budget.

Runtime tree pages use limits 1 through 200 and signed cursors bound to the
project, runtime session/sequence, tree snapshot, filters, and limit.
Observation/capture tools are read-only and non-destructive. Run, pause, and
continue are non-read-only and non-destructive. Stop is non-read-only and
destructive. Every runtime tool is closed-world and project/session bound.
