# Godot Codex MCP sidecar

This workspace contains the production sidecar and Bridge RPC client for the
project-local Godot bridge. It offers Bridge RPC 1.8 and accepts compatible
downgrade to 1.0–1.7 over a Unix socket on macOS or IPv4 loopback TCP on
Windows. The External Codex Beta profile contains exactly 41 closed MCP tools,
four fixed resources, and one scene-summary resource template over persistent
semantics, live editor state, runtime diagnostics/control, explicitly approved
editor transactions, automatic validation, and connection health.

## Components

- `bridge-client` validates private discovery, binds the canonical project and
  editor session, performs mutual HMAC authentication on macOS and Windows,
  consumes editor/runtime/transaction events, and exposes strict typed
  resource, scene, script, runtime, and transaction APIs through Bridge RPC
  1.8 while preserving lower-minor compatibility.
- `index-store` contains the storage-neutral logical schema 1.3 and production
  `segment-v3` implementation for atomic resource, scene, and script
  generations.
- `semantic-model` verifies every chunk and the final snapshot checksum before
  atomically publishing a generation. A stale, syncing, or disconnected
  generation is never returned as current.
- `transactions` owns idempotency, approval coordination, status/event
  reconciliation, compound validation/rollback, no-replay recovery, and the
  bounded project-private journal.
- `mcp-server` implements MCP `2025-11-25` with the frozen 41-tool profile.
  Write routes require standard form elicitation; host approval and semantic
  transaction confirmation remain distinct controls.
- `godot-codex-mcp` owns process lifecycle and stdio transport.
- `godot-codex` owns model-free `setup`, `doctor`, and package operations.

For multiple Codex tasks opened on the same project, the `index.lock` holder
is the only full-access owner. Standbys expose only connection diagnostics with
`project_session_busy` and automatically take over after the owner exits. The
transaction coordinator is activated from the same lease, preventing split
index/transaction ownership. A standby uses a bounded authenticated Bridge
probe instead of requesting a full semantic snapshot; this preserves the real
Bridge/editor health projection even when several Codex host sessions are
started together. Full snapshot replication begins only after lease takeover.

## Build and test

```sh
cargo test --workspace --all-targets --manifest-path godot-codex-mcp/Cargo.toml
cargo clippy --workspace --all-targets --all-features --manifest-path godot-codex-mcp/Cargo.toml -- -D warnings
cargo build --release --manifest-path godot-codex-mcp/Cargo.toml
```

The resource and scene paths can be exercised against a running editor with:

```sh
cargo run --manifest-path godot-codex-mcp/Cargo.toml \
  -p godot-codex-bridge-client --example resource_graph_live -- \
  /absolute/path/to/project

cargo run --manifest-path godot-codex-mcp/Cargo.toml \
  -p godot-codex-resource-indexer --example semantic_coordinator_live -- \
  /absolute/path/to/project
```

The toolchain is pinned to Rust 1.94.1 and `rmcp` 2.2.0. The production bridge
transport supports macOS/Unix UDS and Windows `127.0.0.1` TCP. Both use the
same private token, mutual proof, framing, checksum verification, and MCP
surface.

The project-scoped template is `../.codex/config.toml.example`. Prefer
`godot-codex setup`, which previews a digest-bound merge before writing its
owned table and guidance. Setup writes the verified absolute package launcher
under `GodotCodex/current`; App and IDE startup does not rely on a shell PATH.
Project config is effective only after the exact project is trusted and the
Codex surface is restarted when requested by `godot-codex doctor`.

The sidecar never reads an OpenAI API key or makes model requests. Project
content mutation is limited to prepared, revision-guarded editor transactions
after exact MCP form approval. Compound writes and explicit persistence remain
bounded by their preview, save scope, validation policy, and native Undo
history. Protocol messages go to stdout; operational diagnostics go to stderr
and must not include the token, proof, absolute project path, source text,
prompts, approval material, or unrestricted property values.
