# Godot Codex MCP sidecar

This workspace contains the production sidecar and Bridge RPC client for the
project-local Godot bridge. It offers Bridge RPC 1.7 and accepts downgrade to
1.0–1.6 over a Unix socket on macOS or IPv4 loopback TCP on Windows. Its MCP
surface contains 36 closed tools over persistent semantics, live editor state,
runtime diagnostics/control, and explicitly approved editor transactions.

## Components

- `bridge-client` validates private discovery, binds the canonical project and
  editor session, performs mutual HMAC authentication on macOS and Windows,
  consumes editor/runtime/transaction events, and exposes strict typed
  resource, scene, script, runtime, and transaction APIs through Bridge RPC
  1.7 while preserving lower-minor compatibility.
- `index-store` contains the storage-neutral logical schema 1.2 and production
  `segment-v2` implementation for atomic resource and scene generations.
- `semantic-model` verifies every chunk and the final snapshot checksum before
  atomically publishing a generation. A stale, syncing, or disconnected
  generation is never returned as current.
- `transactions` owns idempotency, approval coordination, status/event
  reconciliation, no-replay recovery, and the bounded project-private journal.
- `mcp-server` implements MCP `2025-11-25` with exactly 36 tools, including
  eleven guarded transaction tools whose apply route requires standard form
  elicitation.
- `godot-codex-mcp` owns process lifecycle and stdio transport.

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

The project-scoped template is `../.codex/config.toml.example`. Install it as
`.codex/config.toml` inside the Godot project the editor actually opens, ensure
the release binary is on `PATH` (or use its absolute path), and trust that exact
project before starting the Codex task.

The sidecar never reads an OpenAI API key or makes model requests. Project
content mutation is limited to one prepared, revision-guarded editor action
after exact MCP form approval; it does not save scenes or patch source files.
Protocol messages go to stdout; operational diagnostics go to stderr and must
not include the token, proof, absolute project path, source text, prompts,
approval material, or property values.
