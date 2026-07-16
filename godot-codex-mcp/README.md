# Godot Codex MCP sidecar

This workspace contains the production sidecar and Bridge RPC client for the
project-local Godot bridge. It offers Bridge RPC 1.2 and accepts downgrade to
1.1/1.0 over a Unix socket on macOS or IPv4 loopback TCP on Windows. The MCP
surface remains the three Sprint 2 read-only editor tools until Sprint 3 static
index ingestion is connected.

## Components

- `bridge-client` validates private discovery, binds the canonical project and
  editor session, performs mutual HMAC authentication on macOS and Windows,
  consumes editor snapshots/events, and exposes a strict typed resource snapshot
  and incremental-delta API for Bridge RPC 1.2.
- `index-store` contains the storage-neutral API and selected segment-store
  implementation from the Stage 2 storage spike. It is not yet connected to the
  resource wire-client or MCP tools.
- `semantic-model` verifies every chunk and the final snapshot checksum before
  atomically publishing a generation. A stale, syncing, or disconnected
  generation is never returned as current.
- `mcp-server` implements MCP `2025-11-25` with exactly three read-only tools.
- `godot-codex-mcp` owns process lifecycle and stdio transport.

## Build and test

```sh
cargo test --workspace --all-targets --manifest-path godot-codex-mcp/Cargo.toml
cargo clippy --workspace --all-targets --manifest-path godot-codex-mcp/Cargo.toml -- -D warnings
cargo build --release --manifest-path godot-codex-mcp/Cargo.toml
```

The resource wire-client can be exercised against a running editor with:

```sh
cargo run --manifest-path godot-codex-mcp/Cargo.toml \
  -p godot-codex-bridge-client --example resource_graph_live -- \
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

The sidecar never reads an OpenAI API key, makes model requests, or mutates the
Godot project. The Stage 3 live example has explicit test-only coordination
flags; ordinary `BridgeClient` calls are read-only. Protocol messages go to
stdout; operational diagnostics must go to stderr and must not include the token,
proof, absolute project path, source text, prompts, or inspector property values.
