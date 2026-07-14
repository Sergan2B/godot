# Sprint 1, Stage 4 — authenticated Bridge RPC lifecycle evidence

**Result:** Passed locally

**Run date:** 2026-07-14

**Timezone:** Asia/Bishkek (UTC+06:00)

**Branch:** `codex/integration`

**Base commit:** `967f948fbc022f1b3185b9a1439cbacafd20b914`

**Source state:** Uncommitted Sprint 1 Stage 3 and Stage 4 worktree

**Fixture:** `tests/codex/fixtures/smoke_project` and short private `/tmp/gcb_*` copies for live macOS UDS tests

This evidence closes only Stage 4 of Sprint 1. The Rust conformance client and canonical redacted trace remain Stage 5 work. MCP, the production sidecar, semantic indexing, revisions beyond the zero session vector, and scene access remain outside Sprint 1.

## Implemented scope

| Area | Result |
|---|---|
| RPC envelope | Authenticated `request` and `cancel` messages validate protocol version, request ID, exact project/session context, method, params, and optional deadline before dispatch; responses contain exactly one of `result` or structured `error` |
| Request identity | Request IDs remain unique for the lifetime of a connection; reuse returns `duplicate_request_id` without altering the original request |
| Connection lifecycle | Per-connection state enforces handshake before RPC, exactly one successful initialize, `not_initialized` before initialize, `already_initialized` after it, and a terminal closing state after shutdown |
| `bridge.initialize` | Returns selected protocol, project/session IDs, versioned capabilities/readiness, hard limits, and the zeroed session-scoped revision vector |
| `bridge.ping` | Returns the exact echo value and enforces the 256-byte UTF-8 limit without logging the value |
| `bridge.capabilities` | Returns `bridge.lifecycle` and `transport.uds` at version `1.0`, both independently marked `ready`, plus current limits |
| `bridge.shutdown` | Flushes `{ "closing": true }` and closes only that RPC connection; editor discovery and other client sessions remain active |
| Main-thread boundary | Valid lifecycle requests enter the existing capacity-64 dispatcher; the main thread keeps its eight-command/2,000 µs per-frame budget and returns only an opaque internal completion ID to the worker |
| Deadlines | Omitted deadline is 5,000 ms, accepted range is `1..30,000`, and the monotonic duration begins immediately after the complete frame is validated |
| Cancellation and expiry | Worker-owned terminal state arbitrates completion/cancel/expiry races; queued work is removed before main-thread execution and late completions are discarded |
| Backpressure and disconnect | At most 64 requests are in flight per connection; saturation returns retryable `overloaded`; disconnect removes non-started work without side effects |
| Shutdown safety | Dispatcher access is invalidated under a mutex before a timeout-detached worker could outlive the editor service |

The transport worker owns all wire responses and terminal transitions. The main thread never sees token/proof data, raw frames, client context, or echo values.

## Lifecycle result

The live UDS test executes:

```text
handshake
  → bridge.initialize
  → bridge.ping
  → bridge.capabilities
  → duplicate and unknown-method checks
  → bridge.shutdown
  → authenticated reconnect to the same editor runtime
```

The race/backpressure test separately proves deadline expiry, client cancellation, abrupt disconnect, and the 64-request in-flight limit without allowing cancelled or expired commands to reach the main-thread handler.

## Verification

Enabled editor build:

```sh
BUILD_NAME=codex .venv/bin/scons \
  platform=macos \
  arch=arm64 \
  target=editor \
  dev_mode=yes \
  dev_build=yes \
  tests=yes \
  vulkan=no \
  accesskit=no \
  angle=no \
  -j8
```

| Check | Result | Evidence |
|---|---|---|
| Enabled editor build | Pass | Strict `-Werror` dev build linked `libmodule_codex_bridge`; exit 0 |
| Targeted bridge tests | Pass | 22/22 cases; 605/605 assertions; 0 failed |
| Full local unit suite | Pass | 1,427/1,427 cases; 421,762/421,762 assertions; 3 skipped; exit 0 |
| Live lifecycle | Pass | Authenticated initialize, ping, capabilities, duplicate, unknown method, shutdown, connection close, and authenticated reconnect |
| Live race/backpressure | Pass | Deadline, cancel, disconnect, and 64-request saturation all removed queued work correctly |
| Race repetition | Pass | Both live UDS RPC cases passed 10 consecutive runs; 2/2 cases and 130/130 assertions per run |
| Enabled editor smoke | Pass | Exit 0 with `Service started` and `Service stopped`; only empty private runtime directories remained |
| Enabled editor SHA-256 | Recorded | `6a537facbc0e7b6e0b5a4851d43400493193598c66261c40fadb094147b1d3d9` |

Opt-out editor build used the same command with `module_codex_bridge_enabled=no`:

| Check | Result | Evidence |
|---|---|---|
| Opt-out build | Pass | Exit 0 with `tests=yes` |
| Symbol and string exclusion | Pass | No service, worker, runtime, handshake, RPC session symbols, or `[codex_bridge]` strings in the binary |
| Runtime exclusion | Pass | Headless editor smoke created no `.godot/codex` directory and emitted no bridge log |
| Opt-out editor SHA-256 | Recorded | `b5e11e9beb7489af685816167ba31ff4545e32b59473c9224f2e261b3517db26` |

Non-editor build:

```sh
BUILD_NAME=codex .venv/bin/scons \
  platform=macos \
  arch=arm64 \
  target=template_debug \
  dev_mode=yes \
  dev_build=yes \
  vulkan=no \
  accesskit=no \
  angle=no \
  -j8
```

| Check | Result | Evidence |
|---|---|---|
| Template build | Pass | Exit 0 |
| Symbol and string exclusion | Pass | No service, worker, runtime, handshake, RPC session symbols, or `[codex_bridge]` strings in the template binary |
| Template SHA-256 | Recorded | `718fffdedb13ee5779ef5b3b7afa987d95c7910ca2bad01a6d4b6000c62ba888` |

All changed implementation, test, and evidence files pass repository formatting, include, code-owner, spelling, copyright, header, and file-format hooks.

## Known observations

- The full Godot suite still emits existing negative-path diagnostics while passing every executed case.
- The project-local macOS `sockaddr_un.sun_path` limitation and the upstream editor shutdown flake remain recorded in [SPRINT-1-STAGE-3.md](SPRINT-1-STAGE-3.md); Stage 4 introduced no new runtime limitation.
- No Rust implementation is claimed by this evidence. Cross-language discovery, schema, fragmentation, failure, and lifecycle conformance belongs to Stage 5.

## Gate assessment

- Authenticated request/response/cancel envelope: **locally complete**.
- Initialize, ping, capabilities, and connection-only shutdown: **locally complete**.
- Deadline, cancellation, exactly-once terminal arbitration, and backpressure: **locally complete**.
- Remote CI and review evidence: **not run**; no push or PR was authorized.
- Sprint 1 overall: **in progress**.

The next implementation gate is Stage 5 of Sprint 1: add the locked Rust conformance client under `tests/codex`, drive discovery/handshake/lifecycle and negative transport cases from the canonical schemas/fixtures, emit a canonical redacted trace, and record final Sprint 1 evidence.
