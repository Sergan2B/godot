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
- `surface-capture` owns the private, one-shot, hash-chained transport tap used
  only while acquiring official App/CLI/IDE acceptance evidence. With no armed
  lease it creates no state and the stdio path is unchanged.
- `godot-codex` owns model-free `setup`, `doctor`, package operations, and the
  JSON-only `surface-capture arm|status|cancel|consume|abandon` control plane.

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

## Official-host acceptance capture

`surface-capture` is acquisition infrastructure, not a general telemetry
mode. `arm` first verifies the exact installed package, setup-owned config,
private receipt, canonical project, and measured metadata. The next matching
sidecar process claims that lease once and records only closed protocol names,
safe allowlisted tool observations, hashes, and revision coordinates. It never
records prompts, source text, raw form content, unrestricted tool results, or
absolute project paths. The `--surface` value is an intended acquisition
label, not a trustworthy App/CLI/IDE process selector: exact package/project
bindings select the claim, and a human operator must attest the official host
that actually owned it.

```sh
godot-codex surface-capture arm \
  --surface app \
  --project-root /absolute/project \
  --metadata /absolute/metadata.json \
  --ttl-seconds 1800 \
  --json

godot-codex surface-capture status \
  --run-id <64-lowercase-hex> \
  --json
```

An authorizing arm result uses
`godot-codex-surface-capture-result/1.1`, has `state: armed`, and has
`disposition: created|recovered`. `recovered` returns the same run after an
exact retry, including a retry caused by losing the first CLI response. A
different active lease, a claimed lease, or an exact expired lease is returned
as a non-authorizing JSON report on stderr with exit status 2; it identifies
the existing `run_id` so the operator can inspect and safely resolve that run
before retrying. Do not start a host for a non-authorizing result.

The official host must close its MCP session normally before `status` becomes
`finalized`; only a `completed` outcome qualifies. An operator SIGINT/SIGTERM
is recorded as `cancelled`, even when the process drains and exits
successfully, and cannot qualify. `cancel` removes only an unclaimed lease.
`consume` removes only an exact finalized run, requires its full-file SHA-256,
and is deferred until all three surface artifacts and final acceptance pass.
After the failed host/sidecar is confirmed stopped, `abandon` can discard only
an exact metadata-digest-bound failed claimed state that cannot be recovered;
it rejects live claims, recoverable partial finalizations, healthy finalized
runs, unsafe layouts, and symlinks. These commands never edit project content,
Codex trust, or Codex configuration. The legacy Python direct-client recorder
is fixture-only and cannot produce qualifying evidence.

The sidecar never reads an OpenAI API key or makes model requests. Project
content mutation is limited to prepared, revision-guarded editor transactions
after exact MCP form approval. Compound writes and explicit persistence remain
bounded by their preview, save scope, validation policy, and native Undo
history. Protocol messages go to stdout; operational diagnostics go to stderr
and must not include the token, proof, absolute project path, source text,
prompts, approval material, or unrestricted property values.
