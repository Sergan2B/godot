# Sprint 1, Stage 2 — editor-only bridge module evidence

**Result:** Passed locally

**Run date:** 2026-07-14

**Timezone:** Asia/Bishkek (UTC+06:00)

**Branch:** `codex/integration`

**Base commit:** `cf61ff8f82d7e0d0037bce12a222eec93c3bc3f4`

**Source state:** Uncommitted Sprint 1 Stage 1 and Stage 2 worktree

**Fixture:** `tests/codex/fixtures/smoke_project`

This evidence closes only Stage 2 of Sprint 1. Discovery files, UDS, authentication, RPC lifecycle, Rust conformance, MCP, semantic indexing, and scene access remain outside this stage.

## Implemented scope

| Area | Result |
|---|---|
| Build integration | `modules/codex_bridge/config.py` permits editor builds only; `SCsub` compiles the module sources; the standard `module_codex_bridge_enabled=no` opt-out remains functional |
| Registration | `CodexBridgeService` is registered through `EditorPlugins` at `MODULE_INITIALIZATION_LEVEL_EDITOR` |
| Service lifecycle | Enter-tree starts the dispatcher and worker; exit-tree and module uninitialization stop them idempotently |
| Main-thread boundary | Typed FIFO commands, capacity 64, cancellation by request ID, deadline filtering, and shutdown rejection/queue clearing |
| Frame budget | At most 8 dequeued commands or 2,000 microseconds per frame |
| Worker lifecycle | Dedicated transport worker scaffold with wakeup semaphore, stop request, bounded one-second wait, and ref-counted timeout-safe context |
| Tests | Godot doctest coverage for capacity, shutdown, FIFO, cancellation, command budget, time budget, deadlines, and repeated worker start/stop |

The transport worker intentionally does not open a listener in this stage. Consequently, startup creates no `.godot/codex` directory or runtime files.

## Shutdown order

1. Move the service to `STATE_STOPPING`.
2. Stop accepting commands and clear pending queue entries.
3. Signal the transport worker and wake it from its wait point.
4. Join it when it exits within the bounded timeout; otherwise detach the thread while its ref-counted context remains valid until worker exit.
5. Move the service to `STATE_STOPPED`.

Connection closure and runtime-file cleanup hooks are intentionally empty until Stage 3 introduces those resources.

## Verification

Enabled editor build:

```sh
BUILD_NAME=codex .venv/bin/scons \
  platform=macos \
  arch=arm64 \
  target=editor \
  dev_mode=yes \
  dev_build=yes \
  vulkan=no \
  accesskit=no \
  angle=no \
  tests=yes \
  -j8
```

| Check | Result | Evidence |
|---|---|---|
| Enabled editor build | Pass | Strict `-Werror` dev build linked `libmodule_codex_bridge`; final incremental build exit 0 |
| Targeted unit tests | Pass | 5/5 cases; 117/117 assertions; 0 failed |
| Full local unit suite | Pass | Exit 0; existing negative-path diagnostic lines remain visible |
| Enabled editor smoke | Pass | Exit 0 with `[codex_bridge] Service started.` followed by `[codex_bridge] Service stopped.` |
| Runtime-file hygiene | Pass | `tests/codex/fixtures/smoke_project/.godot/codex` was not created |
| Enabled editor SHA-256 | Recorded | `a7ee90f546ccd7afeacb9c7846431d16597ae2ec05f5015e908732ab37fc8b81` |

Opt-out editor build used the same command with `module_codex_bridge_enabled=no`:

| Check | Result | Evidence |
|---|---|---|
| Opt-out build with `tests=yes` | Pass | Exit 0; module-aware test guard prevents disabled-module link dependencies |
| Symbol exclusion | Pass | No `CodexBridgeService`, `MainThreadDispatcher`, or `BridgeTransportWorker` symbol in the resulting editor binary |
| Runtime exclusion | Pass | Headless editor run emitted no `codex_bridge` lifecycle log |

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
| Template build | Pass | Exit 0; elapsed 00:01:48.97 |
| Symbol exclusion | Pass | No bridge service, dispatcher, or worker symbol in `bin/godot.macos.template_debug.dev.arm64` |
| Template SHA-256 | Recorded | `a5befff0501a6737e63544a86611dc71dd8ead2b0adc0776a2d51207e90229dc` |

## Known observation

The first enabled headless editor smoke reached bridge shutdown successfully and then reproduced the Sprint 0 upstream macOS shutdown failure at `EditorNode::is_cmdline_mode`. It also printed an unrelated unfinished-thread warning after bridge shutdown. The bridge worker itself had already joined: there was no bridge timeout error before `Service stopped`. Two subsequent enabled smoke runs completed with exit 0 and no crash or bridge warning. The upstream flake remains visible and is not converted to `continue-on-error`.

## Gate assessment

- `BRG-001` editor-only module skeleton and build guards: **locally complete**.
- `BRG-002` service lifecycle and bounded main-thread dispatcher: **locally complete**.
- Remote CI and review evidence: **not run**; no push or PR was authorized.
- Sprint 1 overall: **in progress**.

The next implementation gate is Stage 3: private discovery plus authenticated macOS UDS transport. Stage 3 must preserve the lifecycle, queue, build, and redaction boundaries verified here.
