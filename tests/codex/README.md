# Codex bridge conformance client

This directory contains the Sprint 1 Rust conformance client, the shared Godot
fixture, and the Sprint 2 live smoke harness. It is test infrastructure; the
production sidecar now lives in the repository-root `godot-codex-mcp` workspace.

## Offline checks

The exact stable toolchain is pinned in `rust-toolchain.toml`, and all commands
use the committed lockfile:

```sh
cargo fmt --manifest-path tests/codex/Cargo.toml -- --check
cargo clippy --locked --manifest-path tests/codex/Cargo.toml \
  --all-targets -- -D warnings
cargo test --locked --manifest-path tests/codex/Cargo.toml
cargo build --locked --release --manifest-path tests/codex/Cargo.toml
```

The unit suite parses and validates the canonical JSON Schema/fixture bundle
directly from `schemas/codex_bridge/v1`; it also reproduces the project-ID and
mutual HMAC vectors. Schemas and fixtures are not copied into the Rust source.

## Sprint 3 resource graph contract

Stage 1 adds a storage-independent resource graph oracle under
`fixtures/resource_graph_oracle` and a Godot project under
`fixtures/resource_graph_project`. The fixture covers text/binary resources,
text/binary scenes as resources, an imported SVG, chain/fan-in/fan-out/cycle/orphan
topologies, missing/stale references, equal-content UID-less resources, Unicode paths,
and deterministic mutation phases.

Run the contract-only tests without launching Godot:

```sh
python3 -m unittest tests.codex.test_resource_graph_contract
cargo test --manifest-path tests/codex/Cargo.toml \
  --locked --offline resource_graph_contract
```

Run the complete Draft 2020-12, identity, restore, Godot API projection, mutation, and
redaction gate with an enabled editor build:

```sh
python3 tests/codex/resource_graph_fixture.py validate --all-phases \
  --godot-bin bin/godot.macos.editor.dev.arm64
```

The harness always works in a marked short-path temporary copy. It never mutates the
canonical project during validation and refuses to replace an unmarked directory. The
machine-readable result is written to
`evidence/sprint-3-stage-1-contracts.json`.

## Live suite

Open a short-path copy of `fixtures/smoke_project` in an enabled macOS editor
build, wait for `.godot/codex/bridge.json`, then run:

```sh
cargo run --quiet --locked --manifest-path tests/codex/Cargo.toml -- \
  --project-root /tmp/gcb_fixture/project \
  --trace tests/codex/evidence/sprint-1-trace.json
```

The client validates private discovery, recomputes the canonical project ID,
performs mutual authentication, and exercises the lifecycle and negative
transport suite. The trace is canonical JSON by construction. Secret values,
nonces, proofs, project/session identifiers, and local paths are replaced with
stable redaction markers before an atomic write; a readback scan rejects a
trace containing the observed secrets or canonical project root.

The macOS v1 socket is project-local. Use a short fixture path because
`sockaddr_un.sun_path` has a small platform limit.

## Sprint 2 model-free live slice

The fixture includes an opt-in editor plugin. It is inert during ordinary use
and activates only when the harness sets `CODEX_SPRINT2_AUTOMATION=1`. The
plugin selects `Player`, applies two unsaved `EditorUndoRedoManager` property
changes (`275.0`, then `310.0`), and coordinates with the MCP client without
saving `main.tscn`.

On Windows x86_64, build the release sidecar and run:

```powershell
python tests\codex\sprint2_live_smoke.py `
  --godot bin\godot.windows.editor.dev.x86_64.console.exe `
  --sidecar godot-codex-mcp\target\release\godot-codex-mcp.exe `
  --project-root tests\codex\fixtures\smoke_project `
  --evidence tests\codex\evidence\sprint-2-live-smoke.json `
  --timeout 40
```

The macOS arm64 UDS profile is live-verified as well:

```sh
mkdir -p /tmp/gcb-s2
rsync -a --delete --exclude .godot/ \
  tests/codex/fixtures/smoke_project/ /tmp/gcb-s2/project/
python3 tests/codex/sprint2_live_smoke.py \
  --godot /path/to/Godot.app/Contents/MacOS/Godot \
  --sidecar godot-codex-mcp/target/release/godot-codex-mcp \
  --project-root /tmp/gcb-s2/project \
  --evidence tests/codex/evidence/sprint-2-live-smoke-macos.json
```

The harness launches the editor and sidecar, speaks MCP over stdio, asserts the
exact node/type/owner/script and both live values, requires dirty state and
increasing event/scene revisions, verifies that a new snapshot generation is
used, requires all per-session runtime artifacts to disappear after editor
exit, and atomically writes a normalized evidence artifact. It fails closed on
hosts other than Windows x86_64 and macOS arm64.

## Sprint 3 Stage 3 resource bridge gate

The Stage 3 harness starts a fresh short-path editor project for every Stage 1
oracle phase, consumes the Bridge RPC 1.2 snapshot through the production Rust
client, applies the mutation outside the canonical fixture, and verifies the
resulting delta. It also forces a live oversized batch to prove typed
journal-gap fallback.

```sh
python3 tests/codex/sprint3_stage3_live.py \
  --godot bin/godot.macos.editor.dev.arm64
```

The canonical project digest must remain unchanged. Normalized local evidence
is written to `evidence/sprint-3-stage-3-bridge.json`; remote CI and unrun
platforms are not inferred from this gate.

## Sprint 3 Stage 4 persistent index and resource MCP gate

The Stage 4 gate launches the real editor and release sidecar against fresh
short-path copies of the resource oracle. It verifies all eight mutation phases,
the production segment store, same-session cache reuse, full rebuild after a
journal gap, both resource MCP tools, one-record signed pagination, and direct /
reverse parity without a model:

```sh
python3 tests/codex/sprint3_stage4_index_mcp.py \
  --godot bin/godot.macos.editor.dev.arm64 \
  --timeout 240
```

The canonical macOS arm64 evidence is written to
`evidence/sprint-3-stage-4-index-mcp-macos.json`. It contains raw query,
change-visibility, and status/ping samples with p50/p95 summaries. Windows,
Linux, and remote CI remain explicitly `not_run`; they belong to the later
cross-platform smoke stage.
