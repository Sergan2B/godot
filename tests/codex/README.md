# Codex bridge conformance client

This directory contains the Sprint 1 Rust conformance client and the Godot
fixture used by the C++ and cross-language suites. It is test infrastructure,
not the production `godot-codex-mcp` sidecar planned for Sprint 2.

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
