# PROTOCOL-001 — Bridge RPC 1.0

**Status:** Accepted for Sprint 1

**Date:** 2026-07-14

**Protocol version:** `1.0`

**Decision owner:** `Sergan2B` (interim Sidecar/Protocol and Security owner)

**Canonical schemas:** [`../../schemas/codex_bridge/v1`](../../schemas/codex_bridge/v1)

**Parent documents:** [MASTER_SPRINT_ROADMAP.md](MASTER_SPRINT_ROADMAP.md), Sprint 1; [PRODUCT-001-semantic-bridge-vision-and-plan.md](PRODUCT-001-semantic-bridge-vision-and-plan.md); [ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md](ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md)

## 1. Purpose and scope

Bridge RPC is the private, project-scoped protocol between the editor-only `modules/codex_bridge` module and a local Rust client. Version 1.0 fixes the first compatibility boundary required to implement the Sprint 1 bridge skeleton.

This specification defines:

- macOS Unix Domain Socket discovery and framing;
- project and editor-session identity;
- version negotiation and compatibility;
- token-based mutual authentication;
- request, response, cancellation, deadlines, and terminal-response rules;
- the Sprint 1 lifecycle methods;
- session-scoped revision coordinates;
- structured errors, limits, and redaction requirements;
- canonical JSON Schemas and conformance fixtures.

Snapshots, event notifications, chunks/acks, semantic entities, MCP, indexing, runtime observation, and transactions are outside the implemented Sprint 1 surface. They must extend the authenticated session and compatibility rules defined here rather than introduce a second transport.

## 2. Normative conventions

`MUST`, `MUST NOT`, `SHOULD`, and `MAY` are normative. Sizes are byte counts unless stated otherwise. Integers on the wire are JSON integers in the interoperable range `0..2^53-1`, except the four-byte binary frame prefix.

Protocol constants:

| Constant | Value |
|---|---:|
| Bridge RPC version | `1.0` |
| Handshake schema version | `1.0` |
| Frame prefix | 4-byte unsigned big-endian length |
| Maximum JSON payload | 1,048,576 bytes (1 MiB) |
| Compression | None |
| Session token | Exactly 32 random bytes |
| Client nonce | Exactly 32 random bytes |
| Server nonce | Exactly 32 random bytes per connection |
| Proof | HMAC-SHA-256, 32 bytes |
| Handshake timeout | 3,000 ms |
| Default request deadline | 5,000 ms |
| Maximum request deadline | 30,000 ms |
| Maximum in-flight requests | 64 per connection |
| Main-thread dispatch budget | At most 8 commands or 2,000 µs per frame |
| `bridge.ping` echo | At most 256 UTF-8 bytes |

Binary values in JSON use unpadded base64url as defined by the URL-safe Base64 alphabet. A 32-byte value is therefore exactly 43 characters matching `^[A-Za-z0-9_-]{43}$`.

## 3. Runtime discovery and publication

The macOS v1 runtime layout is project-local:

```text
.godot/codex/
├── bridge.json
├── bridge.lock
├── session.token
└── run/
    └── bridge-<session-hex>.sock
```

The containing directories use mode `0700`. `bridge.json`, `bridge.lock`, `session.token`, and the socket use owner-only access; regular files use mode `0600`. A more permissive observed mode is a startup failure, not a warning followed by publication.

`session.token` contains exactly 32 raw random bytes and no text encoding or trailing newline. It is regenerated for every editor session.

The endpoint path and token are published only after successful UDS `bind()` and `listen()`:

1. acquire the project-local lock without replacing an active owner;
2. create private directories and bind/listen on the session socket;
3. write a token temporary file, set mode `0600`, `fsync` as supported, and atomically rename it to `session.token`;
4. write and atomically rename the discovery record to `bridge.json` last.

The discovery record contains only project-relative paths:

```json
{
  "created_at": "2026-07-14T10:00:00Z",
  "discovery_schema": 1,
  "editor_session_id": "editor:0123456789abcdef0123456789abcdef",
  "endpoint": ".godot/codex/run/bridge-0123456789abcdef0123456789abcdef.sock",
  "pid": 12345,
  "project_id": "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd",
  "protocol_versions": ["1.0"],
  "token_file": ".godot/codex/session.token",
  "transport": "uds"
}
```

The client discovers the record from the canonical project root supplied to that client. It MUST reject absolute paths, `..` traversal, symlinks escaping `.godot/codex`, an unexpected file owner/mode, a mismatched `project_id`, or a changed discovery/session record during connection.

A second editor for the same canonical project MUST NOT replace active discovery. Stale ownership is not inferred from age alone: the implementation checks process state and attempts the authenticated endpoint before taking over. Shutdown stops accept, closes clients, removes `bridge.json`, `session.token`, the socket, and the lock owned by that session, and waits for the worker only for a bounded interval.

## 4. Framing and JSON encoding

Every handshake and RPC message uses the same frame:

```text
0                   1                   2                   3
0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                    payload_length (u32be)                     |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|            payload bytes (exactly payload_length) ...        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

- `payload_length` is the number of following bytes and excludes the prefix.
- A length of zero or greater than `1,048,576` is invalid. The receiver closes the connection without allocating the declared payload.
- The receiver supports arbitrary fragmentation and multiple complete frames in one read.
- The payload is strict UTF-8 JSON with one top-level object. A UTF-8 BOM, trailing non-whitespace bytes, invalid escapes, non-finite numbers, and duplicate object member names are invalid.
- Compression, content-type negotiation, and alternate encodings are not supported in 1.0.
- The length is checked before payload allocation and JSON parsing. Parsed nesting, strings, arrays, and objects remain subject to implementation limits even when the frame is under 1 MiB.

When no trustworthy `request_id` can be recovered (invalid prefix, UTF-8, or JSON), the server closes the connection and emits only a sanitized local diagnostic. It does not echo the payload. Schema-valid RPC requests receive structured errors where possible.

## 5. Project identity

### 5.1. Canonical root on the macOS profile

Both bridge and client compute the canonical root independently:

1. start from the directory containing the selected `project.godot`;
2. require the directory and `project.godot` to exist;
3. resolve symlinks plus `.` and `..` using the operating-system physical canonicalization equivalent to `realpath(3)`;
4. encode the resulting POSIX path exactly as UTF-8 with `/` separators;
5. remove a trailing `/` unless the result is `/`.

Version 1.0 performs no case folding or additional Unicode normalization after physical canonicalization. The identifier is machine/path scoped; it is not promised to remain stable when the same project is moved or opened through a different physical root.

### 5.2. Fingerprint

Let:

```text
D = ASCII("godot-codex-project-id/v1") || 0x00
R = canonical_root encoded as UTF-8
H = SHA-256(D || R)
project_id = "project:sha256:" || lowercase_hex(H)
```

For the synthetic canonical root `/fixtures/codex-smoke`:

```text
project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd
```

The absolute root is never included in discovery, handshake, normal RPC, logs, or traces. The hash prevents direct disclosure but is not treated as a secret or a substitute for the session token.

## 6. Version negotiation and compatibility

Discovery and `handshake.client_hello` advertise the maximum supported minor for each supported major. An implementation advertises at most one entry per major. Support for `1.3` means support for compatible minors `1.0` through `1.3`.

The server selects the highest major supported by both peers and the lower of their maximum minors for that major. Version 1.0 currently advertises only `1.0`.

Compatibility rules:

- A different major is incompatible and returns `protocol_mismatch` during handshake.
- Within one major, a minor release may add optional fields, methods, capabilities, error codes, and enum-like values with a safe unknown fallback.
- A minor release MUST NOT remove or reinterpret a field, add a new required field to an existing message, or change an existing method's side effects.
- Receivers MUST ignore unknown optional fields after applying size/depth limits. They MUST NOT reject a same-major message solely because such a field exists.
- Senders use the negotiated version and MUST NOT assume a new method or behavior without its advertised capability.
- Unknown methods return `method_not_found`; unknown capability/readiness/error values are preserved or surfaced as unknown, not mapped to success.

If no major is compatible, `handshake.error` exposes only `protocol_mismatch`, a safe message, retryability, and the server's supported versions. No project path, capability state, token detail, or editor data is returned.

## 7. Authenticated handshake

### 7.1. Sequence

All handshake messages are framed JSON. The entire sequence from accepted connection through `handshake.server_ready` must complete within 3,000 ms.

```text
client                                      bridge
  | handshake.client_hello                    |
  | versions, project, editor session, nonce ->|
  |                                           | validate binding/version
  |<- handshake.server_challenge              |
  | server nonce, selected version, proof     |
  | verify server proof                       |
  | handshake.client_authenticate ----------->|
  | client proof                              | constant-time verify
  |<- handshake.server_ready                  |
  | authenticated project/session/version     |
```

The client nonce and each server nonce are independently generated 32-byte random values. A nonce MUST NOT be reused. The client reads the 32-byte token only from the verified project-local token file.

Before `server_ready`, only handshake messages are accepted. After `server_ready`, handshake messages are invalid and only RPC envelopes are accepted.

### 7.2. Authenticated transcript

All security-relevant fields from the hello and challenge are bound into one unambiguous binary transcript. Define `LP(x)` as a four-byte unsigned big-endian length followed by `x`, and `COUNT(n)` as a four-byte unsigned big-endian integer.

```text
T = ASCII("godot-codex-bridge/handshake-transcript/v1") || 0x00
    || LP(UTF8(handshake_version))
    || COUNT(number_of_client_supported_versions)
    || LP(UTF8(client_supported_versions[0])) ...
    || LP(UTF8(selected_protocol_version))
    || LP(UTF8(project_id))
    || LP(UTF8(editor_session_id))
    || LP(client_nonce_raw_32)
    || LP(server_nonce_raw_32)
```

Offered versions retain transmitted order. Unknown optional handshake fields cannot alter version selection, project/session binding, nonce interpretation, or proof semantics in major 1; any future security-relevant field requires a new authenticated transcript version.

Proofs use the raw 32-byte session token as the HMAC key:

```text
server_proof = HMAC-SHA-256(
    token,
    ASCII("godot-codex-bridge/server-proof/v1") || 0x00 || T)

client_proof = HMAC-SHA-256(
    token,
    ASCII("godot-codex-bridge/client-proof/v1") || 0x00 || T)
```

Proofs are encoded as unpadded base64url on the wire and compared in constant time after strict decoding to exactly 32 bytes. A client verifies `server_proof` before sending its proof.

The canonical test vector in `schemas/codex_bridge/v1/fixtures/test-vectors/handshake.json` uses:

```text
transcript_sha256 = 57e92eadc54792bd048597950a62a8325a39ddb2f29b707be6a8b3d78d3e6241
server_proof = FA-WdGW6swe0_f-qWeYapRoRqHvPjBQIWI-zkFLUFI8
client_proof = Po5Rvle7JrEO3__pIRWtSyFMNJSFX3XSU6iVRMbUvw0
```

The fixture token is public test data and MUST never be used by a running editor.

### 7.3. Failure behavior

- A project ID or editor session mismatch returns `project_not_bound` or `session_mismatch` and closes the connection.
- A malformed or incorrect proof returns `unauthenticated` and closes the connection. The response does not identify whether the token, nonce, or proof differed.
- A version-major mismatch returns `protocol_mismatch` and supported versions, then closes.
- Timeout, invalid sequencing, replayed nonce/proof, or any unexpected pre-auth message closes the connection with a sanitized diagnostic.
- Authentication failures are rate-limited per endpoint without retaining token/proof/payload data.

## 8. RPC envelope

Every post-handshake message includes the negotiated `protocol_version`, a `kind`, and the authenticated connection context.

### 8.1. Request

```json
{
  "context": {
    "editor_session_id": "editor:0123456789abcdef0123456789abcdef",
    "project_id": "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd"
  },
  "deadline_ms": 5000,
  "kind": "request",
  "method": "bridge.ping",
  "params": { "echo": "ping-1" },
  "protocol_version": "1.0",
  "request_id": "req:0000000000000002"
}
```

`request_id` is unique among all requests seen on a connection, including completed requests. Reuse returns `duplicate_request_id` and does not alter the original request.

The context must exactly match the authenticated project and current editor session. Context mismatch is checked before dispatch.

### 8.2. Response

A response has the same `request_id` and context and contains exactly one of `result` or `error`:

```json
{
  "context": {
    "editor_session_id": "editor:0123456789abcdef0123456789abcdef",
    "project_id": "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd"
  },
  "kind": "response",
  "protocol_version": "1.0",
  "request_id": "req:0000000000000002",
  "result": { "echo": "ping-1" }
}
```

Structured error:

```json
{
  "context": {
    "editor_session_id": "editor:0123456789abcdef0123456789abcdef",
    "project_id": "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd"
  },
  "error": {
    "code": "method_not_found",
    "message": "The requested bridge method is not supported.",
    "retryable": false
  },
  "kind": "response",
  "protocol_version": "1.0",
  "request_id": "req:0000000000000009"
}
```

Error messages are safe, bounded user-facing summaries. Optional `data` contains only versioned remediation metadata, never a token, proof, payload excerpt, property value, or absolute path.

### 8.3. Cancel

```json
{
  "context": {
    "editor_session_id": "editor:0123456789abcdef0123456789abcdef",
    "project_id": "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd"
  },
  "kind": "cancel",
  "protocol_version": "1.0",
  "reason": "client_cancelled",
  "request_id": "req:0000000000000008"
}
```

The `request_id` identifies the request being cancelled; cancel has no separate response. If the request has not produced a terminal response, the server completes it exactly once with `cancelled`. A late cancel for an already-terminal request is ignored. Cancellation and expiry remove queued work before it reaches the main thread.

Notification, ack, and chunk kinds are reserved for later minor-version capabilities and are not accepted by the Sprint 1 implementation.

## 9. Connection and method lifecycle

Connection states are:

```text
accepted → handshaking → authenticated → initialized → closing → closed
```

After handshake, exactly one `bridge.initialize` must succeed before any other method. A duplicate initialize returns `already_initialized`. Another method before initialize returns `not_initialized`.

### 9.1. `bridge.initialize`

Parameters identify the client binary and requested capabilities. They contain no account identity or secrets.

Result:

- selected protocol version;
- `project_id` and `editor_session_id`;
- versioned capabilities and their readiness;
- negotiated/hard limits;
- current session-scoped revision vector.

Sprint 1 advertises at least `bridge.lifecycle` and `transport.uds` version `1.0`.

### 9.2. `bridge.ping`

Returns the supplied `echo` unchanged. The encoded UTF-8 value is limited to 256 bytes. It does not access Godot editor objects or extend the connection deadline implicitly.

### 9.3. `bridge.capabilities`

Returns the same capability/limit representation used by initialize plus current readiness. Capability presence and readiness are separate: a known capability may be `starting`, `unavailable`, `read_only`, or `stopping` rather than `ready`.

### 9.4. `bridge.shutdown`

Returns `{ "closing": true }`, flushes that terminal response, and closes only the authenticated RPC connection. It MUST NOT close Godot Editor, stop another client session, or delete the editor's discovery record.

## 10. Deadlines, queues, and exactly-one completion

`deadline_ms` is a duration measured from the server's monotonic timestamp immediately after the complete request frame is validated. It is not a wall-clock timestamp.

- Omitted `deadline_ms` means 5,000 ms.
- Values outside `1..30,000` are `invalid_request`.
- Expired work returns `deadline_exceeded` and never enters the main-thread queue if expiry is known first.
- At most 64 requests are in flight per connection. A full queue returns `overloaded` without main-thread dispatch.
- The main thread processes at most 8 commands or 2,000 µs per frame, whichever is reached first.
- Disconnect cancels all non-started work. Sprint 1 lifecycle methods have no commit point and produce no post-disconnect side effect.

For every schema-valid accepted request ID, the server records one terminal state and sends at most one terminal response. Cancellation, deadline, shutdown, worker completion, and disconnect race through this single terminal transition. A result that arrives after cancellation/expiry is discarded.

## 11. Structured errors

Sprint 1 defines these codes:

| Code | Meaning | Retryable |
|---|---|---:|
| `unauthenticated` | Handshake proof failed or is incomplete | No; new handshake required |
| `project_not_bound` | Project fingerprint does not match | No |
| `session_mismatch` | Discovery/editor session is stale | After rediscovery |
| `protocol_mismatch` | No compatible major version | After upgrade/downgrade |
| `invalid_frame` | Framing/UTF-8/JSON is invalid | No; connection closes |
| `message_too_large` | Payload exceeds the hard limit | After reducing/chunking |
| `invalid_request` | Envelope, context, deadline, or parameters are invalid | After correction |
| `not_initialized` | A method preceded initialize | After initialize |
| `already_initialized` | Initialize was repeated | No on this connection |
| `method_not_found` | Method is unknown in negotiated capabilities | No unless upgraded |
| `duplicate_request_id` | Request ID was already observed | With a new ID |
| `deadline_exceeded` | Deadline elapsed before completion | Bounded read retry |
| `cancelled` | Client cancellation won the terminal race | At client discretion |
| `overloaded` | In-flight or dispatch queue limit was reached | Yes, with backoff |
| `internal_error` | Safe unexpected server failure | Depends on `retryable` |

Unknown same-major error codes are surfaced as unknown errors while preserving `retryable` and safe remediation data. They are never treated as success.

## 12. Session-scoped revisions

Bridge RPC 1.0 revision counters do not persist across editor restarts:

```json
{
  "editor_session_id": "editor:0123456789abcdef0123456789abcdef",
  "event_seq": 0,
  "operation_seq": 0,
  "project_revision": 0,
  "scene_revisions": {}
}
```

- Every revision vector includes its `editor_session_id`.
- `event_seq` stores the last emitted sequence; zero means no event has been emitted and the first event is 1.
- Numeric values are comparable only when `project_id` and `editor_session_id` match.
- A new editor process creates a new random session ID and may restart every counter from zero.
- A client MUST NOT infer freshness across editor sessions from equal or greater numeric counters.
- Persistent revision epochs, delta replay, and index revisions are deferred. Full snapshot is the future cross-connection recovery primitive.

## 13. Security, privacy, and logs

The transport is local-only but does not trust every local process. UDS permissions, a per-session random token, mutual proof, exact project/session binding, and bounded parsing are all required; none substitutes for another.

The `codex_bridge` log category may record method name, safe error code, duration, sizes, queue depth, shortened opaque session ID, and hashed project ID. It MUST NOT record:

- token bytes or token-file contents;
- client/server nonces or HMAC proofs;
- raw/decoded payloads or echo/property values;
- absolute project paths or environment variables;
- source text, prompts, runtime values, or authorization data.

Authentication comparison uses Godot's constant-time crypto API. Temporary token/proof buffers are cleared when practical. Crash text and assertions use codes and lengths, not secret or payload values.

## 14. Canonical schemas and fixtures

The authoritative bundle is [`schemas/codex_bridge/v1`](../../schemas/codex_bridge/v1):

- `common.schema.json` — shared identifiers, context, limits, revisions, capabilities, and errors;
- `discovery.schema.json` — `bridge.json`;
- `handshake.schema.json` — the five handshake message variants;
- `rpc.schema.json` — request/response/cancel envelopes;
- `lifecycle.schema.json` — Sprint 1 lifecycle params/results;
- `fixture-manifest.schema.json` — conformance case manifest;
- `fixtures/` — positive, negative, fragmentation, compatibility, project-ID, and proof vectors.

Schemas use JSON Schema 2020-12. Unknown properties are intentionally permitted so a compatible minor release can add optional fields. Implementations still apply hard byte/depth/item limits before ignoring an unknown field.

Schema validation is necessary but not sufficient. The conformance client also checks sequencing, negotiated version, duplicate request IDs, exact terminal responses, byte-size limits, HMAC vectors, project binding, cancellation/deadline races, and cleanup.

## 15. Decision closure and acceptance

This specification closes the architecture decisions required before the Sprint 1 skeleton:

| Decision | Resolution |
|---|---|
| D-02 wire encoding/framing | 4-byte big-endian length + strict UTF-8 JSON; 1 MiB; no compression |
| D-03 project fingerprint/revisions | Domain-separated SHA-256 of physical canonical root; revisions are editor-session scoped |
| D-04 authentication | 32-byte token/nonces; mutually verified HMAC-SHA-256 transcript proofs; constant-time comparison |

Protocol 1.0 is ready for implementation when:

- every schema and JSON fixture parses;
- every manifest case resolves to an existing schema/fixture;
- project-ID and HMAC vectors reproduce exactly in C++ and Rust;
- same-major optional-field fixtures are accepted;
- incompatible-major, wrong-project, and wrong-proof cases fail with the specified safe behavior;
- frame fragmentation, zero/oversized length, invalid JSON, request lifecycle, and exactly-one terminal response are covered by the Sprint 1 conformance suite.

## 16. Deferred extensions

Later sprints may add notification, ack, chunk, snapshot, event stream, runtime, and transaction capabilities as compatible minor additions when they remain optional and capability-gated. A change to framing, authentication transcript semantics, project fingerprint inputs, required fields, or existing side effects is a Bridge RPC major-version change.
