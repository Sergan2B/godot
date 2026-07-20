# Sprint 7 plan — full live editor context

**Status:** Complete on the local macOS arm64 acceptance coordinate

**Milestone:** Live Editor Alpha

**Baseline:** `9417794f930b09812e307d9cebd3e14c74e33b74`

**Normative contracts:** [EDITOR-001](EDITOR-001-live-editor-context.md),
[PROTOCOL-001](PROTOCOL-001-bridge-rpc-v1.md),
[MCP-001](MCP-001-project-scoped-read-tools.md), and
[EVIDENCE-001](EVIDENCE-001-semantic-facts-and-evidence.md)

## 1. Outcome and fixed decisions

Sprint 7 adds a session-scoped live overlay above the persistent resource,
scene, and saved-script index. For fields explicitly covered by a complete
editor snapshot, editor state is authoritative and the disk observation is
retained for comparison and evidence.

- Bridge RPC `1.5` is an additive compatible extension with `1.0` through `1.4`
  fallback.
- The persistent logical and physical storage schemas do not change. Live state
  is memory-only and is discarded at editor-session boundaries.
- The MCP registry grows from ten to sixteen read-only tools and adds the fixed
  `godot://editor/summary` resource.
- Viewport support is metadata-only. Pixel capture remains Sprint 8.
- Unsaved script source is not semantically parsed in this sprint. The live
  contract exposes tab identity, dirty state, hashes, and selections only.
- The blocking acceptance coordinate is local macOS arm64. Windows, Linux,
  remote CI, and the model-facing smoke are recorded as `not_run`.

## 2. Gates

| Gate | Deliverable | Exit condition |
|---|---|---|
| `S7-01` | This plan, `EDITOR-001`, protocol and MCP amendments | DTOs, merge, revisions, history, limits, errors, and scope are frozen |
| `S7-02` | Bridge RPC 1.5 and revision transitions | 1.5 contract vectors pass and 1.0–1.4 fixtures remain compatible |
| `S7-03` | Safe bounded Variant projection | Cyclic and oversized fixtures terminate deterministically within limits |
| `S7-04` | Open scenes, selection, and Inspector capture | Clean, dirty, and unsaved tabs are observable without identity loss |
| `S7-05` | Open scripts and native history summaries | Commit, Undo, and Redo advance the promised sequences and revisions |
| `S7-06` | Diagnostics, Output, viewport metadata, and lifecycle | Close, reimport, reconnect, and project invalidation cannot leave stale current state |
| `S7-07` | Sidecar live overlay and disk/editor composer | Complete live facts override disk facts; incomplete coverage creates no false tombstones |
| `S7-08` | Six focused MCP tools and editor summary | Exact sixteen-tool registry, stale guards, signed paging, limits, and redaction pass |
| `S7-09` | Independent fixture/oracle and revision-conflict tests | Oracle does not import production merge/query code and every acceptance scenario passes |
| `S7-10` | macOS live gate and closeout evidence | Rebuilt artifacts pass correctness, recovery, security, cleanup, and SLO gates on one freeze |

## 3. Public surface

Existing editor, scene, node, and find-usages tools gain the additive live
envelope defined by `EDITOR-001`. New read-only tools are:

- `godot_get_open_scenes`;
- `godot_get_inspector_state`;
- `godot_get_open_scripts`;
- `godot_get_editor_history`;
- `godot_get_diagnostics`;
- `godot_get_viewport_state`.

All live reads accept optional `expected_editor_session_id` and
`expected_event_seq`; scene-scoped reads additionally accept
`expected_scene_revision`. A mismatch fails closed with
`stale_editor_state` or `stale_scene_revision` and the current coordinates.
List tools accept limits from 1 through 200 and signed cursors bound to the
project, session, snapshot, filters, and limit.

The fixed `godot://editor/summary` resource has a conservative 4096-byte budget.
The existing scene summary template may include an additive live overlay when
the requested scene is open in the bound editor session.

## 4. Fixture and acceptance

The independent fixture opens a clean saved scene, a dirty saved scene, and an
unsaved scene. It exercises three-node selection, Node/Resource/opaque Inspector
targets, two scripts with a dirty active buffer and multiple selections, native
commit/Undo/Redo, an opaque third-party action, cyclic and oversized values,
bounded Output messages, viewport metadata, scene close, save, reimport, event
gap, reconnect, and session reset.

Sprint 7 passes only when:

1. the unsaved Inspector value is the effective value while disk evidence is
   preserved;
2. dirty state is explicit in scene, editor summary, and revision coordinates;
3. multi-selection preserves each node identity;
4. stale session, event, and scene revisions are rejected;
5. commit, Undo, and Redo monotonically advance `operation_seq` and the
   affected scene revision;
6. unknown native action payloads stay opaque;
7. close, reimport, gaps, reconnect, and project reset leave no stale current;
8. cyclic/large values, diagnostics, Output, and viewport metadata obey their
   redaction and size limits;
9. Bridge RPC 1.4 remains compatible; and
10. live semantic facts replace only fully covered disk facts.

The macOS runner rebuilds the Godot editor and release sidecar, runs C++, Rust,
Python, schema, oracle, lifecycle, cleanup, and redaction checks, and produces
`tests/codex/evidence/sprint-7-live-editor-macos.json` without overwriting an
existing qualifying artifact.

SLOs remain: live current-scene/selection query p95 at most 500 ms, editor
change visibility p95 at most 2 seconds, control ping p95 at most 200 ms, and no
unhandled editor stall beyond the 2000 microsecond dispatcher budget. The editor
summary must stay within 4096 bytes.

## 5. Commit boundaries

1. Contracts and Sprint plan.
2. Bridge RPC 1.5 and revision transitions.
3. Safe cyclic Variant projection.
4. Open scenes, selection, and Inspector capture.
5. Open scripts and native history.
6. Diagnostics, viewport metadata, and lifecycle.
7. Sidecar live overlay and disk/editor composition.
8. Focused MCP tools and editor resource.
9. Independent fixtures and revision gates.
10. macOS evidence and completion documentation.

Each implementation commit builds and passes its relevant tests. The closing
evidence commit does not modify the frozen implementation, oracle, or source
scope.

## 6. Explicitly deferred

Runtime/remote trees, EditorDebugger, stack traces, game process controls,
viewport pixels, screenshots, write tools, transactions, automatic Undo,
unsaved-source semantic parsing, and persistence of live overlay state remain
outside Sprint 7.

## 7. Completion

`S7-01` through `S7-10` are implemented. The qualifying acceptance run is
source-bound by the manifest digest and recorded in
[SPRINT-7-EVIDENCE](SPRINT-7-EVIDENCE.md). It covers the independent fixture and
oracle, Python regressions, the Rust workspace, a tests-enabled Godot editor
build, the complete Codex Bridge C++ test source, the release sidecar, and the
model-free live editor lifecycle on macOS arm64.

Windows, Linux, remote CI, and the model-facing Codex CLI smoke are explicitly
`not_run` and do not block the agreed local gate. Live Editor Alpha is complete;
the next implementation sprint is Sprint 8, Runtime and EditorDebugger.
