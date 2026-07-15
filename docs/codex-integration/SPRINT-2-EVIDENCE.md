# Sprint 2 evidence — first MCP vertical slice

**Recorded:** 2026-07-14 (Windows); 2026-07-15 (macOS)

**Local hosts:** Windows x86_64; macOS 26.5.2 arm64

**Target runtimes:** Windows x86_64 (`tcp_loopback`); macOS arm64 (`uds`)

## Implemented scope

- Bridge RPC 1.1 negotiation, editor snapshot transfer, checksums, acknowledgments,
  bounded event invalidation, and revision vectors;
- main-thread editor adapters for the current scene, selection, unsaved Inspector
  values, NodePath, owner, script, dirty state, and bounded Variant projection;
- the production Rust `godot-codex-mcp` workspace with strict project discovery,
  authenticated BridgeClient, atomic snapshot replication, and stale-state denial;
- Windows protected-DACL runtime files and an ephemeral `127.0.0.1` endpoint
  authenticated with the same per-session mutual HMAC as the Unix profile;
- exactly three read-only MCP tools using protocol `2025-11-25`;
- project-scoped Codex configuration, usage instructions, schema fixtures, and the
  manual live-selection checklist.

Snapshot shaping is bounded before transport allocation: 256 KiB per selected
node, 4 MiB of Inspector data across the snapshot, 1024 characters per identity
field, and a 32 MiB per-client outbound window. The applied limits and
truncation flag propagate through the verified replica into every MCP result.

## Local verification

### Godot editor build

```text
python -m SCons platform=windows target=editor dev_build=yes tests=yes \
  module_codex_bridge_enabled=yes accesskit=no d3d12=no angle=no -j8
```

Result: PASS. The full editor and console binaries linked successfully. A final
incremental build recompiled `test_codex_bridge.cpp`, `codex_bridge_service.cpp`,
and `editor_context_adapter.cpp` after the last source changes.

### macOS editor build and CodexBridge tests

```text
BUILD_NAME=codex .venv/bin/scons platform=macos arch=arm64 target=editor \
  dev_mode=yes dev_build=yes tests=yes vulkan=no accesskit=no angle=no -j8
bin/godot.macos.editor.dev.arm64 --test \
  '--test-case=*[CodexBridge]*' --no-colors
```

Result: PASS. The strict Apple Silicon editor build linked with warnings as
errors. The focused suite passed 26 test cases and 636 assertions with no
failures. Its temporary project now uses short `/tmp` roots on macOS so the
project-local endpoint remains below `sockaddr_un.sun_path`; failed setup is
also guarded from cascading into a test-process crash.

The full macOS Godot suite also passed: 1,431 test cases and 421,793 assertions,
with three declared skips and no failures.

### CodexBridge unit tests

```text
bin/godot.windows.editor.dev.x86_64.console.exe --test \
  --test-case="*[CodexBridge]*" --no-colors
```

Result: PASS — 24 test cases and 617 assertions, with no failures. Windows now
runs the live runtime publication, duplicate ownership, mutual authentication,
negative framing, lifecycle, cancellation, deadline, saturation, and disconnect
tests that were previously Unix-only. The suite also fills the 4096-entry
notification journal and proves the next event is rejected at the negotiated
bound.

### Editor lifecycle smoke

```text
bin/godot.windows.editor.dev.x86_64.console.exe --editor --headless \
  --path tests/codex/fixtures/smoke_project --quit-after 180 --verbose
```

Result: PASS. The editor loaded `main.tscn` and `player.gd`, logged
`[codex_bridge] Service started.`, and stopped the service cleanly at exit.

The opt-in two-phase fixture automation was also exercised against the real
Windows editor APIs. It selected `Player`, applied unsaved values `275.0` and
`310.0` through `EditorUndoRedoManager`, exited cleanly, and byte-compared
`main.tscn` before/after to prove the on-disk value remained `240.0`.

### Production Rust workspace

```text
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo check --workspace --target aarch64-apple-darwin
cargo build --release -p godot-codex-mcp
```

Result: PASS — 15 tests, zero Clippy warnings, successful native Windows and
macOS arm64 compile checks, and a successful optimized sidecar build.

The production workspace was rerun natively on macOS arm64: formatting passed,
14 platform-applicable tests passed, Clippy emitted no warnings under
`-D warnings`, and the optimized `godot-codex-mcp` binary built successfully.
The separate locked Sprint 1 conformance workspace passed its eight offline
schema, fixture, discovery, framing, authentication-vector, and trace tests.

The model-free vertical-slice test feeds the canonical checksum-verified Bridge
RPC 1.1 snapshot into the semantic replica and invokes the selected-nodes MCP
handler. It verifies dirty state, `Player`, `res://player.gd`, the unsaved
`movement_speed` value, and `live_editor_property` evidence.

A separate resync test commits one generation, invalidates it on an event gap,
proves that the old generation is no longer readable as current, and exposes a
newer generation only after all replacement chunks and checksums commit.

BridgeClient also has a routing test for valid and malformed sync notifications
that can arrive while an RPC response or snapshot frame is pending. Valid
events now force immediate resnapshot instead of disconnecting the session.

### MCP stdio smoke

An actual sidecar process received MCP `initialize`, `notifications/initialized`,
`tools/list`, and `tools/call` messages over stdio.

Result: PASS — protocol `2025-11-25`; exactly
`godot_get_current_scene`, `godot_get_editor_state`, and
`godot_get_selected_nodes`; the offline tool call returned the structured
retryable `editor_state_unavailable` execution error.

### Schemas and canonical fixtures

The seven-schema registry, the manifest, and every schema case were validated
with a Draft 2020-12 validator.

```text
PASS schemas=7 cases=51 schema_cases=37
```

The existing Unix conformance harness also passes the following target check:

```text
cargo check --all-targets --target aarch64-apple-darwin
```

It cannot be executed on Windows because its
live discovery and transport tests intentionally use Unix ownership, mode, and
Unix Domain Socket APIs.

### Repository hygiene

```text
git diff --check
```

Result: PASS.

## Windows live end-to-end gate

The model-free harness at `tests/codex/sprint2_live_smoke.py` passed on Windows
x86_64 and produced `tests/codex/evidence/sprint-2-live-smoke.json` from the real
editor→bridge→sidecar→MCP chain.

```text
platform: windows-x86_64
disk value: 240.0
first live value: 275.0, event_seq 4, scene_revision 2
second live value: 310.0, event_seq 5, scene_revision 3
snapshot IDs: distinct
status: pass
```

The run also exposed and verified fixes for three broad live-path issues:
outer frames now preserve full floating-point precision so structured payloads
equal their checksum JSON; dirty state uses the undo history's saved status
instead of the edge-triggered scene-change check; and committed editor actions
invalidate the replica through `history_changed` as well as undo/redo through
`version_changed`.

The editor removed `bridge.json`, `session.token`, and `bridge.lock` on exit,
the scene remained unsaved during both observations, and `main.tscn` was
byte-identical before and after the run.

## macOS live end-to-end gate

The same model-free harness passed on macOS arm64 through the production Unix
Domain Socket profile and wrote
`tests/codex/evidence/sprint-2-live-smoke-macos.json`.

```text
platform: macos-arm64
disk value: 240.0
first live value: 275.0, event_seq 4, scene_revision 2
second live value: 310.0, event_seq 5, scene_revision 3
snapshot IDs: distinct
status: pass
```

The editor binary was built from this tree with SHA-256
`cbff618c8049f481a58623ba4097350b6c2868b45479cec4dfb4780088de1cb6`;
the release sidecar SHA-256 was
`b2deec40779bdee29b4ba4d58f84c5961c18e8ddf1b835fd60fcbe4ad494bc7f`.
After editor exit, `bridge.json`, `session.token`, `bridge.lock`, and the UDS
endpoint were absent; only the empty private `codex/run` directories remained.
The copied fixture's `main.tscn` was byte-identical before and after the run.

## Optional human-facing Codex gate

The final human gate then uses an external Codex task. Follow
`SPRINT-2-USAGE.md`: select `Player`, change `movement_speed` without saving,
query the selection without revealing its path/value in the prompt, and verify
the exact NodePath, `res://player.gd`, unsaved value, dirty state, live evidence,
and a monotonically increasing scene revision after a second edit.
Archive the prompt, tool calls, normalized responses and UX capture using
`tests/codex/evidence/sprint-2-live-checklist.md`.

The automated production transport gate is complete on Windows x86_64 and
macOS arm64. The external Codex checklist remains useful as a separate optional
UX acceptance record and does not block the Sprint 2 Architecture Proof.
