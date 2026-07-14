# Godot Codex MCP sidecar

This workspace is the production Sprint 2 sidecar for the project-local Godot
bridge. It connects only to the authenticated Bridge RPC 1.1 endpoint for the
supplied project root — a Unix socket on macOS or IPv4 loopback TCP on Windows
— and exposes three read-only MCP tools over stdio.

## Components

- `bridge-client` validates private discovery, binds the canonical project and
  editor session, performs mutual HMAC authentication on macOS and Windows,
  and consumes full snapshots plus ordered invalidation events.
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

The toolchain is pinned to Rust 1.94.1 and `rmcp` 2.2.0. The production bridge
transport supports macOS/Unix UDS and Windows `127.0.0.1` TCP. Both use the
same private token, mutual proof, framing, checksum verification, and MCP
surface.

The project-scoped template is `../.codex/config.toml.example`. Install it as
`.codex/config.toml` inside the Godot project the editor actually opens, ensure
the release binary is on `PATH` (or use its absolute path), and trust that exact
project before starting the Codex task.

The sidecar never reads an OpenAI API key, makes model requests, or mutates the
Godot project. Protocol messages go to stdout; operational diagnostics must go
to stderr and must not include the token, proof, absolute project path, source
text, prompts, or inspector property values.
