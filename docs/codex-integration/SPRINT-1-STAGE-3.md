# Sprint 1, Stage 3 — private discovery and authenticated macOS UDS evidence

**Result:** Passed locally

**Run date:** 2026-07-14

**Timezone:** Asia/Bishkek (UTC+06:00)

**Branch:** `codex/integration`

**Base commit:** `967f948fbc022f1b3185b9a1439cbacafd20b914`

**Source state:** Uncommitted Sprint 1 Stage 3 worktree

**Fixture:** `tests/codex/fixtures/smoke_project`, copied to a short private `/tmp` path for live macOS UDS smoke tests

This evidence closes only Stage 3 of Sprint 1. RPC lifecycle, the Rust conformance client, MCP, the production sidecar, semantic indexing, revisions, and scene access remain outside this stage.

## Implemented scope

| Area | Result |
|---|---|
| Project identity | The project root is resolved with `realpath(3)`, must contain `project.godot`, and is hashed with the `godot-codex-project-id/v1` domain separator without publishing the absolute path |
| Private runtime | `.godot/codex` and `run` are `0700`; lock, discovery, token, and socket are owner-only `0600`; unexpected owners, modes, file types, and symlinks fail startup |
| Publication | The project lock is acquired first, UDS `bind()`/`listen()` completes before secrets are published, `session.token` and `bridge.json` use private temporary files plus `fsync` and atomic rename, and discovery is published last |
| Session rotation | Every editor session receives a new 32-byte raw token, editor session ID, and session-named socket |
| Duplicate and stale ownership | Kernel `flock` prevents concurrent publication; a second worker preserves authenticated discovery; stale takeover checks process state and an authenticated endpoint before removing inactive artifacts |
| Framing | Four-byte unsigned big-endian length, 1 MiB maximum, pre-allocation length rejection, fragmentation, and multiple frames per read |
| Strict JSON | Top-level object, strict UTF-8, no BOM/NUL/duplicate keys, escaped-equivalent duplicate detection, and pre-parser depth/container limits |
| Authentication | Mutual HMAC-SHA-256 over the complete version/project/session/nonce transcript, separate proof domains, strict unpadded base64url, constant-time proof comparison, and a three-second handshake deadline |
| Failure behavior | Safe errors for project, session, version, and proof failures; replayed client nonces and invalid sequencing are rejected; authentication failures are rate-limited without retaining payloads or proofs |
| Shutdown | Clients and listener close before session-owned discovery, token, socket, and lock are removed; the worker retains the bounded shutdown contract from Stage 2 |

The transport accepts only the handshake in this stage. Post-authentication RPC envelopes intentionally remain unavailable until Stage 4.

## Runtime layout and publication order

```text
.godot/codex/
├── bridge.json
├── bridge.lock
├── session.token
└── run/
    └── bridge-<session-hex>.sock
```

`bridge.json` contains only the project fingerprint, process/session metadata, protocol versions, and project-relative endpoint/token paths. Token, proofs, frame payloads, and the canonical project root are not logged.

Publication order is:

1. Validate/create private runtime directories and acquire the project lock.
2. Bind and listen on the private session socket.
3. Generate and atomically publish the 32-byte session token.
4. Atomically publish `bridge.json` last.

Cleanup reverses ownership publication and removes only artifacts that still belong to the current editor session.

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
| Targeted bridge tests | Pass | 17/17 cases; 272/272 assertions; 0 failed |
| Full local unit suite | Pass | 1,422/1,422 cases; 421,429/421,429 assertions; 3 skipped; exit 0 |
| Canonical protocol vector | Pass | Project fingerprint, transcript SHA-256, server proof, and client proof match `schemas/codex_bridge/v1` fixtures |
| Live UDS publication | Pass | Listener, discovery, lock, and token observed while the editor was active; directories were `0700`, artifacts were `0600`, and the token was exactly 32 bytes |
| Enabled editor smoke | Pass | Exit 0 with `Service started` and `Service stopped`; only empty `.godot/codex` and `run` directories remained |
| Enabled editor SHA-256 | Recorded | `33447790f2e5833c23cdeff268a29c15e3e74f779f1cd39ca01ce337014b11de` |

The focused suite covers atomic publication and token rotation, exact ownership/modes, duplicate editors, permissive stale artifacts, inactive stale takeover, authenticated endpoint probing, handshake success and server-proof verification, wrong project/version/proof, nonce replay, timeout, invalid frame lengths, fragmented/multiple frames, strict JSON, and cleanup.

Opt-out editor build used the same command with `module_codex_bridge_enabled=no`:

| Check | Result | Evidence |
|---|---|---|
| Opt-out build | Pass | Exit 0 with `tests=yes` |
| Symbol and string exclusion | Pass | No bridge service, worker, runtime, handshake symbols, or `[codex_bridge]` log strings in the binary |
| Runtime exclusion | Pass | Headless editor smoke created no `.godot/codex` directory and emitted no bridge log |
| Opt-out editor SHA-256 | Recorded | `8adde380305ad6790302b2f1b01ba2ed671cfd3bbaebdeb614d701dcc88a2bef` |

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
| Symbol and string exclusion | Pass | No bridge service, worker, runtime, handshake symbols, or `[codex_bridge]` log strings in the template binary |
| Template SHA-256 | Recorded | `718fffdedb13ee5779ef5b3b7afa987d95c7910ca2bad01a6d4b6000c62ba888` |

All changed C++ and test files passed the repository formatting, include, code-owner, spelling, copyright, header, and file-format hooks. Documentation hooks are rerun after this evidence update.

## Known observations

- macOS limits the absolute `sockaddr_un.sun_path`. A project whose canonical root makes the project-local endpoint too long receives a safe startup failure. Live transport smoke therefore copies the fixture to a short `/tmp` path; no alternate global socket location is introduced silently.
- One live-inspection run reproduced the known upstream macOS editor shutdown failure at `EditorNode::is_cmdline_mode` after the bridge had removed all session artifacts. Subsequent enabled smoke runs exited 0 with clean bridge start/stop. The flake is recorded and is not masked with `continue-on-error`.
- The full Godot suite emits existing negative-path diagnostics while still passing all 1,422 executed cases.

## Gate assessment

- Private project-bound discovery and atomic session publication: **locally complete**.
- Authenticated macOS UDS framing and mutual handshake: **locally complete**.
- Remote CI and review evidence: **not run**; no push or PR was authorized.
- Sprint 1 overall: **in progress**.

The next implementation gate is Stage 4 of Sprint 1: authenticated Bridge RPC request/response/cancel envelopes, exactly-once terminal responses, initialization state, deadlines, cancellation, and the `bridge.initialize`, `bridge.ping`, `bridge.capabilities`, and `bridge.shutdown` lifecycle methods.
