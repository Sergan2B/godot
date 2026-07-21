# EditorDebugger lifecycle and runtime revisions

**Status:** Implemented Sprint 8 workstream

**Wire profile:** Bridge RPC `1.6`

**Parent contract:** [RUNTIME-001](RUNTIME-001-debugger-and-runtime-observation.md)

## 1. Authority and ownership

`EditorRunBar` and `EditorRun` own the local child-process lifecycle.
`EditorDebuggerNode` and the active `ScriptEditorDebugger` own debugger
connectivity and confirmed pause/continue state. `RuntimeDebuggerAdapter`
normalizes those sources into one runtime state machine. `BridgeRevisionClock`
is the only writer of runtime revision coordinates.

Exactly one debugger session may be selected. If two debugger sessions are
simultaneously active, the runtime becomes `failed` with
`runtime_ambiguous`; no process, object, or control target is guessed.

## 2. Lifecycle state machine

States are `starting`, `running`, `paused`, `stopping`, `disconnected`,
`stopped`, `crashed`, `failed`, and `timed_out`. Before the first session the
runtime is logically inactive and no runtime coordinates are present.

| Evidence | Previous state | Result |
|---|---|---|
| accepted MCP run or observed manual run | inactive or terminal | `starting` |
| debugger `started` | `starting` | `running` |
| debugger `breaked(true)` | `running` | `paused` |
| debugger `breaked(false)` | `paused` | `running` |
| explicit editor/MCP stop | `running` or `paused` | `stopping` |
| debugger stop after explicit/remote-normal quit | nonterminal | `stopped` |
| debugger loss while the child is still playing | `running` or `paused` | `disconnected` |
| debugger reconnect within two seconds | `disconnected` | `running` plus full-snapshot invalidation |
| debugger recovery timeout | `disconnected` | `crashed` |
| unexpected process exit | `running` or `paused` | `crashed` |
| debugger startup deadline | `starting` | `timed_out` |
| editor rejects the accepted run | `starting` | `failed` |

A terminal transition wins exactly once. Forced child cleanup occurs only
after the terminal state has been recorded, so synchronous stop callbacks
cannot rewrite `timed_out` as `crashed` or `crashed` as `stopped`.

## 3. Runtime coordinates

`runtime_session_id` is allocated from 16 cryptographically random bytes for
every accepted run attempt. It is allocated before the editor run call, so an
attempt that later fails or times out still has its own identity.

`runtime_event_seq`:

- starts at `1` on `session_started`;
- advances on every externally visible runtime transition or observation;
- is meaningful only together with its `runtime_session_id`;
- resets to `1` only when a new session ID is allocated;
- remains present for retained terminal state;
- is cleared on editor/Bridge shutdown.

Every runtime change also advances the editor-wide `event_seq`. Runtime
changes do not advance persistent resource, scene, script, or project graph
revisions.

## 4. Run and control rules

MCP run accepts only `project` or `current_scene`. It refuses active runtime,
unsaved scene/script content, absent saved current scene, absent main scene,
or unavailable run controller. The read-only launch path calls the regular
build/run integration but does not invoke editor autosave.

Pause and continue complete only after debugger confirmation. Stop completes
after debugger or process lifecycle confirms that the child is no longer
playing. Guards compare the requested session first and the optional expected
sequence second.

| Operation | Deadline | Stable timeout |
|---|---:|---|
| run | 10 s | `runtime_start_timeout` |
| pause / continue | 3 s | `runtime_control_timeout` |
| stop | 5 s | `runtime_control_timeout` |
| observation | 3 s | `runtime_request_timeout` |

Only one request of each control kind may be pending. A duplicate control is
rejected without replacing the original request ID.

## 5. Manual stop and process cleanup

`EditorRunBar::request_stop_playing()` publishes explicit user intent before
calling the internal cleanup path. The editor Stop button and
`EditorInterface.stop_playing_scene()` use this method. Automatic dead-child
cleanup continues to call `stop_playing()` directly and therefore cannot mark
an unexpected exit as a normal stop.

Remote debugger `request_quit` is normal-quit evidence even when the editor
has not yet removed the child process. Internal forced stop during startup or
disconnect timeout suppresses the user-intent signal.

## 6. Disconnect, reconnect, and invalidation

Debugger loss immediately enters `disconnected`, retires the live tree,
objects and correlation state, and fails pending live observations with
`runtime_disconnected`. A two-second nonblocking grace window follows.

Same-session reconnect:

1. preserves `runtime_session_id`;
2. publishes `runtime.invalidated` with `reason: reconnected` and the last
   contiguous sequence;
3. publishes the subsequent `debugger_connected` runtime event;
4. requires a checksum-verified full runtime snapshot before live data is
   served again.

Journal gaps, journal overflow, session replacement, cancellation, and failed
recovery also retire cached runtime data and require a full snapshot.

## 7. Exactly-once terminal arbitration

Bridge RPC owns the external request terminal. The runtime adapter stores only
the internal request IDs currently dispatched to Godot. Deadline,
cancellation, connection loss, debugger callback, and process exit race as
terminal sources; after one removes the RPC pending record, late completion
returns `ERR_DOES_NOT_EXIST` and is discarded.

The adapter clears its matching request ID when a cancellation command reaches
the main thread. Late debugger state still updates lifecycle truth, but it
cannot send a second response for the cancelled request.

## 8. Verification map

| Requirement | Authoritative evidence |
|---|---|
| session isolation and revision clearing | `CodexS8Runtime` revision-clock test |
| stop/crash/disconnect/reconnect classification | `CodexEDRLifecycle` policy test |
| timeout codes and late-completion rejection | `CodexS8BridgeProfile` RPC test |
| manual and MCP lifecycle | model-free Sprint 8 live gate |
| active-run and stale-sequence rejection | model-free Sprint 8 live gate |
| terminal-coordinate retention | model-free Sprint 8 live gate |
| crash and hung-child recovery | model-free Sprint 8 live gate |
| reconnect cache invalidation | Rust runtime-overlay tests |
| no source-content writes | fixture source digest before/after live gate |

The qualifying evidence is produced only from a clean, source-bound macOS
arm64 coordinate. Windows, Linux, remote CI, and model-facing validation are
reported independently and are never inferred from the local gate.
