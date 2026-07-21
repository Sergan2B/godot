# Sprint 8 plan — Runtime and EditorDebugger

**Status:** Complete; qualified by the source-bound local macOS gate

**Milestone:** Diagnostic MVP / M2

**Baseline:** `b0f6627a20e6b7a2128f9152b73e326e1662001a`

**Normative contracts:** [RUNTIME-001](RUNTIME-001-debugger-and-runtime-observation.md),
[PROTOCOL-001](PROTOCOL-001-bridge-rpc-v1.md),
[MCP-001](MCP-001-project-scoped-read-tools.md), and
[EVIDENCE-001](EVIDENCE-001-semantic-facts-and-evidence.md)

## 1. Outcome and fixed decisions

Sprint 8 adds a memory-only runtime projection above the persistent index and
the Sprint 7 live-editor overlay. It observes one game launched by the bound
editor, exposes bounded diagnostics, and never mutates project content.

- Bridge RPC `1.6` is an additive compatible extension with `1.0` through
  `1.5` fallback.
- Manual editor runs and MCP-initiated runs are both observable. More than one
  game/debugger session is `runtime_ambiguous`; the bridge never guesses.
- A run request while a game is active returns `runtime_already_active` and
  never performs an implicit restart.
- Every accepted run attempt receives a fresh `runtime_session_id`, including
  attempts that later fail or time out.
- MCP run requests fail with `runtime_unsaved_changes` rather than invoking an
  editor save. Generated `.godot` state, build caches, and temporary files are
  not project-content writes.
- Runtime tree, object values, and screenshots are retired at a terminal
  boundary. Bounded terminal diagnostics and stacks remain in memory only
  until the next run or editor shutdown.
- The persistent logical and physical storage schemas do not change.
- Runtime property mutation, live-edit messages, write tools, transactions,
  and automatic Undo remain deferred.
- The blocking acceptance coordinate is local macOS arm64. Windows, Linux,
  remote CI, and the model-facing smoke are reported as `not_run`.

## 2. Gates and commit boundaries

| Gate | Deliverable | Exit condition |
|---|---|---|
| `S8-01` | This plan, RUNTIME-001, protocol/MCP amendments | State machine, DTOs, limits, errors, source mapping, retention, and annotations are frozen |
| `S8-02` | Bridge RPC 1.6 schemas, fixtures, negotiation, capabilities, revisions | 1.6 vectors pass and all 1.0-1.5 cases remain compatible |
| `S8-03` | Checked no-autosave run path, runtime clock, debugger observation hooks | Manual and MCP lifecycle transitions are observable without source writes |
| `S8-04` | Correlated bounded remote tree and object inspection | Large, cyclic, and oversized observations terminate deterministically without raw IDs |
| `S8-05` | Runtime diagnostics, stack capture, and terminal retention | Deliberate warnings/errors expose the expected project-relative frames |
| `S8-06` | Viewport capture with path, rate, and size controls | PNG limits pass; no path, temp file, or native handle escapes |
| `S8-07` | Strict Rust runtime client, overlay, recovery, source mapping | Event gaps and new sessions retire stale data; D-09 evidence passes |
| `S8-08` | Nine MCP tools, diagnostics scope, runtime summary | Exact 25-tool registry, closed schemas, annotations, guards, cursors, and image output pass |
| `S8-09` | Independent fixture/oracle and crash/hang/large-data tests | Oracle does not import production mapping/query code and all scenarios reproduce |
| `S8-10` | macOS arm64 gate and source-bound closeout evidence | Rebuilt artifacts pass correctness, recovery, security, cleanup, and SLO gates |

Each implementation commit builds and passes its relevant tests. The closing
evidence commit does not modify the frozen implementation, fixture, oracle, or
validator.

### S8-01 — Freeze RUNTIME-001 and the Sprint boundary

**Depends on:** Sprint 7 closeout.
**Changes:** this plan, RUNTIME-001, the protocol/MCP amendments, the explicit
deferred list, lifecycle authority, retention boundary, D-09 join, errors,
limits, and local-only acceptance coordinate.
**Tests/review:** contract review must account for every state, transition,
terminal reason, public DTO, tool annotation, and forbidden identifier.
**Done when:** no implementation decision about write authority, identity,
freshness, timeout, or truncation remains implicit.

### S8-02 — Add the Bridge RPC 1.6 profile

**Depends on:** S8-01.
**Changes:** additive 1.6 negotiation, runtime capabilities and limits,
optional runtime revision coordinates, eight request methods, two
notifications, runtime snapshot entities, strict schemas, and positive and
named-negative vectors. 1.0–1.5 responses continue to omit runtime fields.
**Tests:** canonical Draft 2020-12 bundle, strict Rust deserialization,
handshake downgrade, closed parameter objects, raw-ID/path rejection, stable
start/control/observation deadline codes.
**Done when:** the 1.6 schema bundle and C++/Rust protocol profiles agree and
all earlier minor versions remain compatible.

### S8-03 — Observe EditorDebugger lifecycle and provide checked run controls

**Depends on:** S8-02.
**Changes:** a memory-only runtime adapter connected to `EditorRunBar`,
`EditorDebuggerNode`, and `ScriptEditorDebugger`; fresh session allocation;
manual-run discovery; checked no-autosave current-scene/project run paths;
pause, continue, stop, disconnect grace, crash, and startup timeout handling.
Manual user stop intent is distinct from automatic dead-child cleanup.
**Tests:** revision-clock isolation, live MCP and manual starts, distinct IDs,
confirmed pause/continue/stop, stale guards, crash versus stop classification,
and a hung child that does not block editor queries.
**Done when:** each accepted run has one session and every request has exactly
one bounded terminal result.

### S8-04 — Capture bounded remote tree and properties

**Depends on:** S8-03.
**Changes:** correlated game-side debugger messages, deterministic depth-first
tree prefix, opaque object IDs, editor-visible property enumeration, bounded
Variant projection, cycle references, typed omissions, and retirement of all
live identities at a terminal boundary.
**Tests:** 10,001-node fixture, 1,200-entry cycle, oversized strings/arrays,
checksum-safe multi-chunk snapshots, stale-object rejection, no raw ObjectID,
RID, PID, pointer, or absolute path.
**Done when:** tree and object observations terminate within their byte,
depth, item, and time budgets before transport serialization.

### S8-05 — Retain diagnostics and stack traces safely

**Depends on:** S8-03.
**Changes:** output/warning/error observation, redaction, repeat and byte
limits, one-based project-relative frames, opaque diagnostic/stack IDs,
pause-stack capture, and bounded terminal diagnostic retention.
**Tests:** deliberate three-function error chain, warning, project-relative
paths, non-project frame removal, crash retention, next-session retirement.
**Done when:** an error stack is available without pausing and terminal state
cannot make stale stack evidence appear current.

### S8-06 — Add safe viewport capture

**Depends on:** S8-03.
**Changes:** single-flight/rate guard, canonical temp-path validation,
regular-file and non-symlink checks, load-and-delete behavior, fit to 1280x720,
iterative PNG byte limiting, digest metadata, and MCP image content without a
second structured byte copy.
**Tests:** GUI PNG signature/digest/size, unavailable headless viewport,
oversized input, unsafe callback path, and forbidden handle/path scan.
**Done when:** pixels leave only as bounded PNG bytes and no temporary path or
native handle crosses either API boundary.

### S8-07 — Build the strict Rust runtime overlay and D-09 join

**Depends on:** S8-02, S8-04, S8-05.
**Changes:** strict runtime DTO/client, notification watcher, memory-only
overlay, full-resnapshot recovery, signed-cursor generation binding, terminal
retention/retirement, and the three-source runtime-to-scene/editor join.
**Tests:** fixture DTO parsing, screenshot verification, session/event gaps,
cursor invalidation, unique mapping evidence, ambiguous/dynamic fail-closed
mapping, and runtime revision-coordinate validation in persistent snapshots.
**Done when:** stale runtime data cannot enter the persistent index or be
served after a session/revision change.

### S8-08 — Publish the MCP Runtime MVP surface

**Depends on:** S8-07.
**Changes:** nine runtime tools, runtime diagnostics scope, editor-state
runtime summary, `godot://runtime/summary`, closed schemas, annotations,
session/sequence guards, pagination, and image output.
**Tests:** exact 25-tool registry, annotation matrix, closed-world inputs,
4096-byte summary, stale/tampered cursor rejection, error translation, and no
subscription/write surface.
**Done when:** Codex can complete the diagnostic workflow using only MCP and
no runtime mutation method is reachable.

### S8-09 — Freeze the independent fixture and recovery oracle

**Depends on:** S8-03–S8-08.
**Changes:** hashed fixture manifest, saved and instanced nodes, dynamic node,
oversized/cyclic values, deterministic warning/error chain, visual marker,
large-tree/crash/hang commands, manual editor commands, golden truth, strict
fixture validator, and model-free live runner.
**Tests:** manifest closure, negative oracle tests, headless and GUI workflows,
large-tree cached-page SLO, crash retirement, hang timeout and cleanup.
**Done when:** the oracle imports no production mapping/query code and can
independently reject weakened limits or false mappings.

### S8-10 — Qualify macOS arm64 and close the sprint

**Depends on:** all previous tasks committed.
**Changes:** no implementation changes; run the source-bound acceptance
wrapper and add only `tests/codex/evidence/sprint-8-runtime-macos.json`.
**Tests:** Python policy regressions, Rust fmt/test/clippy, schema conformance,
tests-enabled editor build, S8 C++ profiles, release sidecar, headless live,
GUI live, process cleanup, redaction scan, artifact/source hashes.
**Done when:** the qualifying evidence validates against the current clean
source coordinate and Windows/Linux/remote/model gates remain honestly
`not_run`.

## 3. Commit boundaries

| Boundary | Allowed content | Required gate before commit |
|---|---|---|
| `C8-1 contracts` | S8-01/02 docs, schemas, vectors, negotiation/profile tests | schema conformance + `CodexS8BridgeProfile` |
| `C8-2 editor runtime` | S8-03–06 editor/game-side C++ and focused tests | tests-enabled editor build + `CodexS8Runtime` |
| `C8-3 sidecar/MCP` | S8-07/08 Rust DTO, overlay, tools, recovery | fmt + workspace tests + clippy |
| `C8-4 fixture/gate` | S8-09 fixture, oracle, runners, policy tests, scope manifest | Python regressions + headless/GUI live |
| `C8-5 evidence` | S8-10 evidence JSON only | `sprint8_acceptance.py --validate` |

A boundary never mixes generated evidence with the implementation it claims
to qualify. If any qualifying run requires a source fix, the evidence is
discarded, the fix enters the appropriate implementation boundary, and the
complete gate runs again from a clean source coordinate.

## 4. Public surface

Bridge RPC 1.6 adds `runtime.run`, `runtime.stop`, `runtime.pause`,
`runtime.continue`, `runtime.snapshot.get`, `runtime.object.inspect`,
`runtime.stack.get`, and `runtime.viewport.capture`, plus `runtime.event` and
`runtime.invalidated` notifications. Exact DTOs, guards, lifecycle semantics,
and limits are defined by RUNTIME-001.

The MCP registry grows from sixteen to twenty-five tools. New observation
tools are `godot_get_runtime_tree`, `godot_inspect_runtime_object`,
`godot_get_stack_trace`, and `godot_capture_viewport`. New control tools are
`godot_run_project`, `godot_run_current_scene`, `godot_stop_project`,
`godot_pause_project`, and `godot_continue_project`.

`godot_get_diagnostics` gains an additive `scope` of `editor` or `runtime`,
defaulting to `editor`. `godot_get_editor_state` gains a bounded runtime
summary, and `godot://runtime/summary` is a fixed 4096-byte resource.

Observation and capture tools are annotated read-only and non-destructive.
Run, pause, and continue are non-read-only and non-destructive. Stop is
non-read-only and destructive because it terminates ephemeral runtime state.

## 5. Fixture and acceptance

The fixture provides a known saved tree, an instanced scene, a runtime-only
node, bounded and oversized properties, a deliberate warning/error with a
fixed call chain, a pause-visible counter, a visual viewport marker, a normal
exit, a crash mode, and a main-thread hang mode.

Sprint 8 passes only when:

1. MCP launches the clean fixture without changing tracked project files;
2. the runtime tree and object inspection contain only opaque identities;
3. deliberate failure returns correct one-based project-relative stack frames;
4. stop plus a new run produces a different runtime session ID;
5. source and live-editor mappings are emitted only with runtime-confirmed evidence;
6. pause, continue, and stop are reported only after debugger confirmation;
7. manual runs are observed and multi-session ambiguity fails closed;
8. crash/disconnect retires tree and values but retains bounded diagnostics;
9. a hung game times out without blocking bridge ping or stop;
10. 10,001-node and oversized-value fixtures truncate deterministically;
11. screenshot output is PNG at most 1280x720 and 512 KiB, with no path or handle;
12. gaps, reconnect, and editor reset require a full runtime resnapshot; and
13. all Bridge RPC 1.0-1.5 and Sprint 7 regressions continue to pass.

The local runner rebuilds the tests-enabled Godot editor and release sidecar,
runs C++, Rust, Python, schema, lifecycle, cleanup, and redaction checks, and
produces `tests/codex/evidence/sprint-8-runtime-macos.json` without overwriting
an existing qualifying artifact.

The qualifying command is:

```sh
.venv/bin/python tests/codex/sprint8_acceptance.py --timeout 60
```

The runner refuses a dirty Sprint 8 source scope and an existing evidence
path. It executes these stages in order:

1. validate the hashed fixture/oracle and its named-negative regressions;
2. check Rust formatting, run the complete workspace tests and deny-warning
   Clippy pass;
3. validate the canonical Bridge schema/fixture bundle;
4. rebuild the tests-enabled macOS arm64 editor and run both S8 C++ profiles;
5. rebuild the release sidecar;
6. run the complete model-free workflow headless, including expected capture
   unavailability;
7. rerun it with a GUI viewport and verify the returned PNG;
8. verify SLOs, unique sessions, cleanup and redaction, bind source and binary
   SHA-256 coordinates, then atomically publish evidence.

SLOs are cached runtime page/diagnostic query p95 at most 500 ms, transition
visibility p95 at most 2 seconds, viewport capture at most 3 seconds, control
ping during a hung game p95 at most 200 ms, and no unhandled editor stall
beyond the 2000 microsecond dispatcher budget.

## 6. Explicitly deferred

Runtime mutation, live-debug create/delete/reparent, arbitrary run arguments,
custom scene paths, expression evaluation, stack locals, persistent runtime
history, scene/script writes, transactions, automatic Undo, Windows/Linux
qualification, hosted CI, and model-facing validation remain outside Sprint 8.
