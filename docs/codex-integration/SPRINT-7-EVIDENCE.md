# Sprint 7 evidence — full live editor context

**Status:** Accepted locally on macOS arm64; Live Editor Alpha achieved

**Source freeze:** `e0ac1441c5e38f1b39dc5cd812e50b82827988b4`

**Closeout date:** 2026-07-20

## 1. Outcome

Sprint 7 gates `S7-01`–`S7-10` are complete. Bridge RPC 1.5 now carries a
session-scoped live overlay for open scene tabs, multi-selection, Inspector,
open scripts, native Undo/Redo history, editor diagnostics, and viewport
metadata. The sidecar composes complete editor observations over disk state and
retains the disk value for comparison.

The blocking acceptance path is local, deterministic, and model-free on macOS
arm64. It rebuilt the tests-enabled Godot editor and release sidecar before
exercising a real headless Godot → Bridge → sidecar → MCP session. Windows,
Linux, remote CI, and the model-facing smoke were not run and are not
represented as passing.

## 2. Canonical artifact

| Artifact | SHA-256 | Bytes | Result |
|---|---|---:|---|
| [`sprint-7-live-editor-macos.json`](../../tests/codex/evidence/sprint-7-live-editor-macos.json) | `e12b7f0cc160aa18633ff8c79ee2dafe8e585328773da9b2e2fb94a5c4c0e5cc` | 39,562 | **PASS** |

The report binds 203 tracked files through
`sha256:f08f354133a8745b22460a76f54ee5dfff7ed55f8e62e32fc683ebd850add102`.
It also binds the fixture manifest, golden live-editor oracle, live runner,
rebuilt Godot binary, and release sidecar by SHA-256.

## 3. Acceptance result

| Gate | Result |
|---|---|
| Independent fixture/oracle | **PASS** |
| Python fail-closed evidence regressions | **PASS** |
| Full Rust workspace tests, formatting, and clippy `-D warnings` | **PASS** |
| Tests-enabled macOS Godot editor rebuild | **PASS** |
| Complete Codex Bridge C++ test source | **PASS** — 61 cases, 9,453 assertions |
| Release sidecar rebuild | **PASS** |
| Model-free live editor lifecycle | **PASS** |
| macOS arm64 | **PASS** |
| Windows / Linux / remote CI / model-facing smoke | `not_run` |

All 18 semantic and lifecycle checks passed. The live value `21.5` overrides
the disk value `8.0` while preserving the latter as comparison evidence. Two
open scenes, three independently identified selected nodes, two open scripts,
explicit dirty state, a redacted diagnostic, and metadata-only 2D viewport
state were observed.

Native commit, 10 Undo/Redo pairs, and an opaque third-party action advanced
the operation sequence from 1 to 22 and the affected scene revision from 3 to
23. The unaffected scene revision remained stable. Stale editor/session/scene
guards, a cursor crossing a snapshot revision, sidecar reconnect, scene close,
and editor-session reset all failed closed or recovered as required.

## 4. Budgets and SLOs

| Measurement | Observed | Limit |
|---|---:|---:|
| Selection query p95 | 0.581 ms | 500 ms |
| Editor change visibility p95, 21 native samples | 669.766 ms | 2,000 ms |
| Control ping p95 | 0.326 ms | 200 ms |
| Editor summary read p95 | 0.215 ms | 500 ms |
| Editor summary | 2,297 bytes | 4,096 bytes |
| Bridge busy-frame p95 | 767 µs | 2,000 µs |
| Bridge busy-frame maximum | 1,588 µs | 2,000 µs |
| Bridge frames over budget | 0 | 0 |

Bounded/cyclic Variant regressions, Output truncation and redaction, closed
input schemas, the exact sixteen-tool registry, read-only annotations, and
disabled subscriptions passed. Cleanup removed all editor/sidecar processes,
temporary workspace data, and Bridge runtime files. The evidence contains no
absolute host paths, endpoint, token, fixture secret, or script source bytes.

## 5. Qualification boundary

The installed CLI was detected as `codex-cli 0.145.0-alpha.18`, but the optional
model-facing smoke is `not_run` because no isolated MCP profile was configured.
The deterministic local gate remains authoritative.

Windows and Linux are not Sprint 7 acceptance coordinates, and no Git-hosted CI
is configured. These explicit `not_run` results do not claim cross-platform or
release qualification. Sprint 8 may now begin with Runtime and EditorDebugger;
write operations and transactions remain deferred to later sprints.
