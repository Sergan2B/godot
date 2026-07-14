# Codex Bridge RPC v1 schemas

This directory is the canonical machine-readable contract for Bridge RPC major version 1. The current protocol version is `1.1`; `1.0` remains supported for the Sprint 1 lifecycle surface. [PROTOCOL-001](../../../docs/codex-integration/PROTOCOL-001-bridge-rpc-v1.md) is the normative behavioral specification.

## Files

- `common.schema.json` — shared identifiers, connection context, errors, capabilities, limits, and revision vectors.
- `discovery.schema.json` — project-local `.godot/codex/bridge.json` for macOS
  UDS and Windows IPv4 loopback TCP.
- `handshake.schema.json` — framed authentication messages.
- `rpc.schema.json` — request, response, and cancel envelopes.
- `lifecycle.schema.json` — `bridge.initialize`, `bridge.ping`, `bridge.capabilities`, and `bridge.shutdown` payloads.
- `sync.schema.json` — capability-gated editor snapshots, chunks, acknowledgements, and ordered editor events added in Bridge RPC 1.1.
- `fixture-manifest.schema.json` — metadata for schema, framing, sequence, and cryptographic conformance cases.
- `fixtures/manifest.json` — the ordered conformance case index.
- `fixtures/valid` and `fixtures/invalid` — wire-message examples.
- `fixtures/test-vectors` — deterministic project-ID and handshake proof vectors.

Schemas use JSON Schema 2020-12. `additionalProperties` is intentionally enabled for protocol objects: compatible minor versions may add optional fields, and a major-1 receiver must ignore unknown optional fields after enforcing byte/depth/item limits.

Schema validation does not replace protocol-state validation. The conformance suite must additionally check handshake ordering, negotiated versions, project/session matching, request-ID uniqueness, deadlines/cancellation, exactly one terminal response, frame limits/fragmentation, and cleanup.

All tokens, nonces, proofs, paths, PIDs, and session IDs in fixtures are synthetic public test data. They must never be used by a running editor or copied into runtime defaults.
