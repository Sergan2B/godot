# Sprint 1, Stage 5 — Rust conformance and final local evidence

**Result:** Passed locally

**Run date:** 2026-07-14

**Timezone:** Asia/Bishkek (UTC+06:00)

**Branch:** `codex/integration`

**Base commit:** `967f948fbc022f1b3185b9a1439cbacafd20b914`

**Implementation commits:** `8face1cae3` (C++ transport/RPC) and `e44e079bb3` (Rust conformance/trace)

**Evidence state:** This document and the roadmap status are commit-bound by the documentation commit that contains them

**Fixture:** `tests/codex/fixtures/smoke_project`, copied to short private `/tmp/gcb5*` paths for live macOS UDS runs

This evidence closes the local implementation and verification work for Sprint
1. The production sidecar, MCP, semantic indexing, scene access, snapshots, and
non-zero revision/event streams remain Sprint 2 or later work. Source commits
were created locally. Push, pull request, and remote CI were not created because
those repository publication actions require separate authorization.

## Delivered artifacts

| Artifact | Result |
|---|---|
| Locked Rust crate | `tests/codex/Cargo.toml`, `Cargo.lock`, and exact stable `rust-toolchain.toml` under the test-only subtree |
| Conformance client | Native `codex-bridge-conformance` binary with no interpreter/runtime requirement; production sidecar behavior is not included |
| Shared contract use | JSON Schema 2020-12 documents and fixtures are loaded directly from `schemas/codex_bridge/v1`; no schema copy or private Godot header is present in Rust |
| Strict JSON/framing | Four-byte big-endian framing, 1 MiB bound, strict top-level object parsing, UTF-8/BOM/trailing-data checks, and duplicate-member rejection |
| Secure discovery | Physical project root, project-ID recomputation, current owner, exact `0700`/`0600` modes, file types, traversal/symlink containment, token length, endpoint type, and discovery stability are checked before use |
| Mutual handshake | Independent client nonce, transcript construction, server HMAC verification, client HMAC generation, exact project/session/version binding, and strict base64url decoding |
| Lifecycle and failures | Initialize, ping, capabilities, connection-only shutdown, wrong project/token/version, fragmentation, invalid framing/JSON, unknown method, duplicate ID, cancellation, deadline, saturation, and abrupt-disconnect recovery |
| Canonical trace | `tests/codex/evidence/sprint-1-trace.json`; sorted canonical JSON with stable redaction markers and atomic write/readback verification |

The only `unsafe` block is the narrowly scoped macOS/Unix `geteuid()` adapter.
Its invariant is stated beside the call, unsafe code remains denied by default,
and a targeted unit test verifies the result against files owned by the test
process.

## Live conformance result

The live suite executes against a running enabled editor:

```text
private discovery
  → fragmented mutual handshake
  → bridge.initialize
  → bridge.ping
  → bridge.capabilities
  → bridge.shutdown and connection close
  → negative handshake/framing cases
  → RPC duplicate/unknown/cancel/deadline/saturation cases
  → abrupt disconnect
  → authenticated reconnect and healthy ping
```

`cargo run` reports **54 passed checks**: all 39 canonical manifest cases,
private live discovery, and 14 additional live cross-language cases. The trace
contains 16 explicit passed case records because related manifest and live RPC
checks are grouped where one connection proves several invariants.

The lifecycle sequencing case additionally requires `not_initialized` before
initialize, `already_initialized` after the first successful initialize, exact
round-trip at 256 UTF-8 bytes, rejection at 258 UTF-8 bytes, and rejection of
deadlines outside `1..30000`.

The deadline burst requires at least one `deadline_exceeded` terminal response
and rejects duplicate terminal responses. The saturation burst sends 256
coalesced frames, which guarantees that a transport read contains more than the
64-request in-flight/dispatcher capacity; at least one safe retryable
`overloaded` response is required. Abrupt disconnect is followed by a new
authenticated connection, initialize, and successful ping.

## Canonical trace and redaction

Trace source: [`../../tests/codex/evidence/sprint-1-trace.json`](../../tests/codex/evidence/sprint-1-trace.json)

| Check | Result |
|---|---|
| Canonical serialization | Pass; object keys and formatting are deterministic, with no timestamp or random trace field |
| Cross-session reproducibility | Pass; traces from different editor sessions and fixture paths compare byte-for-byte equal |
| Token/proof/nonce redaction | Pass; observed secret values are collected before redaction and forbidden by the final readback scan |
| Identity/path redaction | Pass; project ID, editor session ID, endpoint/path fields, and the physical fixture root use stable placeholders |
| Pattern scan | Pass; no `/tmp/gcb5*` root, raw project/session identifier, or unpadded 32-byte base64url value is present |
| Size | 47,462 bytes |
| SHA-256 | `3c52940f695b77cab5a53c4ec5f397d6d96bba3513f24b27c05f12243ee32597` |

Echo text in the canonical trace is deliberate non-secret conformance fixture
data. Runtime payloads, project content, token data, and local editor values are
never copied into the trace.

## Rust verification and provenance

Pinned compiler:

```text
rustc 1.94.1 (e408947bf 2026-03-25)
host: aarch64-apple-darwin
LLVM version: 21.1.8
```

Commands:

```sh
cargo fmt --manifest-path tests/codex/Cargo.toml -- --check
cargo clippy --locked --manifest-path tests/codex/Cargo.toml \
  --all-targets -- -D warnings
cargo test --locked --manifest-path tests/codex/Cargo.toml
cargo build --locked --release --manifest-path tests/codex/Cargo.toml
```

| Check | Result | Evidence |
|---|---|---|
| Formatting and lint | Pass | `rustfmt` clean; Clippy all targets with warnings denied |
| Rust unit tests | Pass | 8/8 passed; strict JSON, discovery path/UID, schema/fixture bundle, vectors, framing, project ID, and trace redaction |
| Locked release build | Pass | Optimized native arm64 Mach-O; exit 0 |
| Runtime dependencies | Pass | Only macOS system `libiconv` and `libSystem`; no installed Rust, Cargo, interpreter, TLS, or async runtime required to execute the binary |
| Lockfile SHA-256 | Recorded | `68e583a64b58e7972304f00ba46d9a9418053d2e8b09f491c6872509185b89e7` |
| Release binary SHA-256 | Recorded | `09011361f1aa2d631b1613c68c014035112898a3e62a29a8026292a73dd054de` |

All dependencies are crates.io releases resolved by `Cargo.lock`; there are no
Git or external path dependencies. `cargo metadata --locked` reports 111
target-inclusive registry packages and no missing license expression. Direct
dependencies and their resolved licenses are:

| Dependency | Resolved | Purpose | License |
|---|---:|---|---|
| `base64` | 0.22.1 | Strict unpadded base64url | MIT OR Apache-2.0 |
| `getrandom` | 0.4.3 | OS client nonce generation | MIT OR Apache-2.0 |
| `hmac` | 0.12.1 | Handshake HMAC-SHA-256 | MIT OR Apache-2.0 |
| `jsonschema` | 0.47.0 | Canonical JSON Schema 2020-12 validation | MIT |
| `libc` | 0.2.186 | Narrow effective-UID platform adapter | MIT OR Apache-2.0 |
| `serde` | 1.0.228 | Custom duplicate-rejecting JSON visitor | MIT OR Apache-2.0 |
| `serde_json` | 1.0.150 | JSON values and canonical serialization | MIT OR Apache-2.0 |
| `sha2` | 0.10.9 | Project fingerprint and transcript hashing | MIT OR Apache-2.0 |

Default network/TLS/async features of `jsonschema` are disabled. The two
resolved `getrandom` versions are target-inclusive transitive graph entries;
the conformance client directly selects 0.4.3.

## Godot regression and shutdown verification

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
| Enabled editor build | Pass | Strict developer build up to date; exit 0 |
| Focused bridge suite | Pass | 22/22 cases; 605/605 assertions |
| Full local Godot suite | Pass | 1,427/1,427 cases; 421,762/421,762 assertions; 3 skipped; exit 0 |
| Live Rust conformance | Pass | Two final release-binary runs on different editor sessions and physical fixture roots; 54 checks each; traces compare byte-for-byte equal |
| Graceful editor cleanup | Pass | The release-binary run ended via editor `--quit-after`; `Service started`/`Service stopped`; discovery, token, socket, and lock absent; only empty private directories remain |
| Enabled editor SHA-256 | Recorded | `6a537facbc0e7b6e0b5a4851d43400493193598c66261c40fadb094147b1d3d9` |

The opt-out editor and non-editor template evidence remains unchanged from
[SPRINT-1-STAGE-4.md](SPRINT-1-STAGE-4.md): the bridge is absent from those
targets, and the template binary SHA-256 remains
`718fffdedb13ee5779ef5b3b7afa987d95c7910ca2bad01a6d4b6000c62ba888`.

The full Godot suite emits the already recorded negative-path UDS,
`OptimizedTranslation`, and drawing diagnostics while returning success. No
failure is hidden with `continue-on-error`.

## Acceptance audit

| Sprint 1 requirement | Current evidence |
|---|---|
| Editor-only module and opt-out/non-editor exclusion | Stage 2 plus unchanged Stage 4 binary evidence |
| Private discovery, duplicate/stale ownership, rotating secret, cleanup | Stage 3 C++ tests plus Rust discovery and graceful cleanup run |
| Mutual project-bound authentication and version negotiation | Canonical vectors, C++ tests, and Rust wrong project/token/version live cases |
| Fragmentation, zero/maximum/oversized frames, invalid JSON | Rust live framing cases and C++ codec tests |
| Initialize → ping → capabilities → shutdown | Rust fragmented live lifecycle and canonical trace |
| Initialization sequencing and byte/deadline limits | Rust live `not_initialized`/`already_initialized`, 256/258-byte UTF-8 ping, and invalid deadline cases |
| Unknown method and duplicate request ID | Rust live RPC responses and C++ session tests |
| Cancellation, deadline, saturation, abrupt disconnect | Rust coalesced live bursts/reconnect plus C++ deterministic queue tests |
| Exactly one terminal response | Rust response-ID uniqueness check plus C++ completion/cancel/expiry race tests |
| Redacted canonical trace | Byte-stable trace, observed-secret scan, path/identity pattern scan, recorded hash |
| Full local editor/test build | Enabled strict build, focused suite, and full suite passed |

## Known limitations and next gates

- Sprint 1 transport remains macOS-only. The project-local endpoint can exceed
  `sockaddr_un.sun_path`; live fixtures therefore use short private paths.
- The Rust crate is conformance-only. It does not expose MCP, index content,
  scenes, runtime observation, write transactions, or OpenAI behavior.
- The implementation commits exist only on the local `codex/integration`
  branch. Push, remote CI, code review, and pre-merge evidence remain `not_run`
  until separately authorized.

## Gate assessment

- Sprint 1 local implementation and verification: **complete**.
- Sprint 1 local source commits: **complete**.
- Sprint 1 publication and remote CI: **not run**.
- Sprint 2 implementation: **not started**.

The next implementation milestone is Sprint 2: the production Rust sidecar and
first read-only semantic bridge slice, without reopening the Sprint 1 transport
contract. Push, pull request, and remote CI remain separately authorized
publication gates.
