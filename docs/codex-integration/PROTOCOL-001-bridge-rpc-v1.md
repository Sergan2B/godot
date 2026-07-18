# PROTOCOL-001 — Bridge RPC 1.x

**Status:** Bridge RPC 1.0 accepted; compatible 1.1–1.3 extensions implemented through Sprint 4 protocol freeze

**Date:** 2026-07-18

**Protocol versions:** `1.0` baseline; `1.1`, `1.2` fallback; `1.3` current

**Decision owner:** `Sergan2B` (interim Sidecar/Protocol and Security owner)

**Canonical schemas:** [`../../schemas/codex_bridge/v1`](../../schemas/codex_bridge/v1)

**Parent documents:** [MASTER_SPRINT_ROADMAP.md](MASTER_SPRINT_ROADMAP.md), Sprint 1; [PRODUCT-001-semantic-bridge-vision-and-plan.md](PRODUCT-001-semantic-bridge-vision-and-plan.md); [ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md](ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md)

## 1. Purpose and scope

Bridge RPC is the private, project-scoped protocol between the editor-only `modules/codex_bridge` module and a local Rust client. Version 1.0 fixes the first compatibility boundary required to implement the Sprint 1 bridge skeleton.

This specification defines:

- local IPC discovery and framing: Unix Domain Sockets on macOS and loopback
  TCP on Windows;
- project and editor-session identity;
- version negotiation and compatibility;
- token-based mutual authentication;
- request, response, cancellation, deadlines, and terminal-response rules;
- the Sprint 1 lifecycle methods;
- session-scoped revision coordinates;
- structured errors, limits, and redaction requirements;
- canonical JSON Schemas and conformance fixtures.

Bridge RPC 1.1 adds capability-gated editor snapshots, event notifications,
chunks, and acknowledgments on the same authenticated session. Bridge RPC 1.2
adds the ResourceUID/dependency graph. Bridge RPC 1.3 adds authoritative
`PackedScene`/`SceneState` observations and project context. MCP, persistent
indexing, runtime observation, and transactions remain outside the bridge wire
surface.

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

The v1 runtime layout is project-local:

```text
.godot/codex/
├── bridge.json
├── bridge.lock
├── session.token
└── run/
    └── bridge-<session-hex>.sock
```

On macOS, the containing directories use mode `0700`; `bridge.json`,
`bridge.lock`, `session.token`, and the socket use owner-only access and regular
files use mode `0600`. On Windows, `.godot/codex`, `run`, and the three runtime
files use a protected DACL limited to the current user, Local System, and local
Administrators. Reparse points are rejected. A more permissive observed access
policy is a startup failure, not a warning followed by publication.

`session.token` contains exactly 32 raw random bytes and no text encoding or trailing newline. It is regenerated for every editor session.

The endpoint and token are published only after a successful local `bind()` and
`listen()`:

1. acquire the project-local lock without replacing an active owner;
2. create private directories and bind/listen on the session endpoint;
3. write a token temporary file, set mode `0600`, `fsync` as supported, and atomically rename it to `session.token`;
4. write and atomically rename the discovery record to `bridge.json` last.

The macOS discovery record contains only project-relative file paths:

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

Windows publishes the same record with a canonical IPv4 loopback endpoint:

```json
{
  "created_at": "2026-07-14T10:00:00Z",
  "discovery_schema": 1,
  "editor_session_id": "editor:0123456789abcdef0123456789abcdef",
  "endpoint": "127.0.0.1:49152",
  "pid": 12345,
  "project_id": "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd",
  "protocol_versions": ["1.1", "1.0"],
  "token_file": ".godot/codex/session.token",
  "transport": "tcp_loopback"
}
```

The Windows server asks the OS for an ephemeral port and binds only
`127.0.0.1`; wildcard, LAN, IPv6, zero, non-canonical, or out-of-range
endpoints are rejected by the client. The TCP stream still requires the same
per-session mutual HMAC authentication before any RPC data is accepted.

The client discovers the record from the canonical project root supplied to
that client. It MUST reject absolute token paths, `..` traversal, symlinks or
reparse points escaping `.godot/codex`, an unexpected Unix owner/mode, a
mismatched `project_id`, a transport/endpoint mismatch, or a changed
discovery/session record during connection. On Windows, the bridge validates
and protects the DACL before publishing; the client independently rejects
reparse points and non-regular runtime objects before reading them.

A second editor for the same canonical project MUST NOT replace active
discovery. Stale ownership is not inferred from age alone: the implementation
checks process state and attempts the authenticated endpoint before taking
over. Unix uses a non-blocking file lock; Windows opens `bridge.lock` with
exclusive read/write sharing. Shutdown stops accept, closes clients, removes
`bridge.json`, `session.token`, the Unix socket when applicable, and the lock
owned by that session, and waits for the worker only for a bounded interval.

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

### 5.1. Canonical root

Both bridge and client compute the canonical root independently:

1. start from the directory containing the selected `project.godot`;
2. require the directory and `project.godot` to exist;
3. resolve symlinks plus `.` and `..` using the operating-system physical canonicalization equivalent to `realpath(3)`;
4. encode the resulting POSIX path exactly as UTF-8 with `/` separators;
5. remove a trailing `/` unless the result is `/`.

On macOS the canonical identity is the physical POSIX path. On Windows both
peers resolve the physical directory, remove the Win32 extended-length
`\\?\` prefix, convert separators to `/`, preserve the filesystem-reported
path casing, and remove the trailing separator. UNC roots are normalized from
`\\?\UNC\server\share` to `//server/share`.

Version 1.0 performs no additional case folding or Unicode normalization after
physical canonicalization. The identifier is machine/path scoped; it is not
promised to remain stable when the same project is moved or opened through a
different physical root.

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

Notification, ack, and chunk kinds are rejected by a negotiated 1.0 session and
are accepted by a negotiated 1.1 session only for the capabilities in section
17.

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

Every session advertises `bridge.lifecycle` plus its active transport:
`transport.uds` on macOS or `transport.tcp_loopback` on Windows, version `1.0`.

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

The transport is local-only but does not trust every local process. Unix
permissions or Windows protected DACLs, loopback-only binding where applicable,
a per-session random token, mutual proof, exact project/session binding, and
bounded parsing are all required; none substitutes for another.

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
- `rpc.schema.json` — request/response/cancel/notification/ack/chunk envelopes;
- `lifecycle.schema.json` — Sprint 1 lifecycle params/results;
- `sync.schema.json` — Bridge RPC 1.1 full snapshots and ordered invalidation events;
- `resource.schema.json` — Bridge RPC 1.2 resource snapshot/delta projection;
- `scene.schema.json` — Bridge RPC 1.3 scene snapshot/delta and project-context projection;
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

Later sprints may add incremental semantic deltas, runtime, and transaction
capabilities as compatible minor additions when they remain optional and
capability-gated. A change to framing, authentication transcript semantics,
project fingerprint inputs, required fields, or existing side effects is a
Bridge RPC major-version change.

## 17. Bridge RPC 1.1 editor synchronization

Bridge RPC 1.1 retains every 1.0 lifecycle method and adds these capabilities:

| Capability | Version | Meaning |
|---|---:|---|
| `editor.context` | `1.0` | Live editor, scene, node, selection, dirty-state projection |
| `editor.inspector` | `1.0` | Bounded live editor-visible properties for selected nodes |
| `sync.full_snapshot_v1` | `1.0` | Atomic full snapshot transfer |
| `sync.event_stream_v1` | `1.0` | Ordered invalidation events after a snapshot boundary |

The server selects `1.1` when the client offers major 1 with maximum minor 1 or
greater. A client offering maximum minor 0 receives `1.0` and the four new
capabilities are not advertised. Discovery publishes `1.1` and `1.0` so legacy
clients can select their compatible maximum.

### 17.1. Snapshot sequence

After initialize, a 1.1 client requests `editor.snapshot.get` with one or both
domains `editor_context` and `editor_inspector`. A successful transfer is
strictly ordered:

```text
client                         bridge
  | editor.snapshot.get          |
  |----------------------------->|
  |<------------- response result| snapshot_id, base event, revisions
  |<----------- snapshot.begin   | chunk count and identical revisions
  |<----------- chunk 0..N-1     | canonical payload JSON + SHA-256
  |<----------- snapshot.end     | entity count + aggregate SHA-256
  | snapshot ack --------------->|
```

Each chunk checksum is lowercase SHA-256 of the exact UTF-8 `payload_json`.
The end checksum is lowercase SHA-256 of the concatenated lowercase chunk
checksum strings in chunk-index order. The client MUST stage the generation,
verify contiguous indices, all checksums, entity count, snapshot identity, and
an identical revision vector, then publish the generation atomically. A failed
transfer MUST NOT replace the current generation.

Snapshots are captured only on the editor main thread. The adapter exposes
session-scoped opaque scene/node identifiers, current `NodePath`, Godot type,
owner/script paths, dirty state, selection, and selected-node inspector values.
Variant projection is bounded to depth 8, 1000 container items, 64 KiB per
projected property, 256 KiB of inspector projection per selected node, 4 MiB
of inspector projection across the snapshot, 1024 characters per identity
field, and 1000 scene nodes. Truncation is explicit.

### 17.2. Ordered events and recovery

Every editor event increments `event_seq`. Scene or property mutations also
increment `project_revision` and the current scene revision; selection-only
changes do not. The bridge emits `sync.event` with `selection_changed`,
`scene_changed`, or `property_changed` and the complete resulting revision
vector.

The Sprint 2 event payload is an invalidation signal rather than a semantic
delta. On the next contiguous event, the sidecar marks the active generation
stale and requests another atomic snapshot. A sequence gap, journal overflow,
session change, or `sync.invalidated` has the same recovery rule. Tools MUST
NOT label the prior generation current while recovery is in progress.

The outbound event journal is bounded to 4096 entries and 16 MiB. A client
outbound window is bounded to 32 MiB. Overflow produces `sync.invalidated`
where possible and never permits silent partial freshness.

### 17.3. Additional limits

Negotiated 1.1 limits add `hard_message_bytes = 8388608`,
`snapshot_chunk_bytes = 524288`, `event_journal_entries = 4096`,
`event_journal_bytes = 16777216`, `snapshot_window_bytes = 33554432`,
`variant_depth = 8`, and `container_items = 1000`. The ordinary framed payload
limit remains 1 MiB. Because a chunk carries both its structured payload and
the exact canonical JSON checksum input, Sprint 2 targets 256 KiB of entity
payload within the negotiated 512 KiB chunk ceiling.

## 18. Bridge RPC 1.2 resource graph

Bridge RPC 1.2 retains 1.0/1.1 and advertises `resource.uid_dependencies` and
`resource.incremental_index`. `resource.snapshot.get` transfers the complete
resource graph in the authenticated snapshot sequence; `resource.delta.get`
replays one exact batch after `after_resource_revision` or returns current/gap.
`resource_graph_changed` and `resource_invalidated` drive incremental recovery.
The canonical strict DTO and limits are in `resource.schema.json` and
`INDEX-001`.

## 19. Bridge RPC 1.3 scene graph

Bridge RPC 1.3 retains every lower-minor capability and additionally advertises
`scene.packed_state`, `scene.incremental_index`, and `scene.project_context`.
The new methods are `scene.snapshot.get` and `scene.delta.get`; both use domain
`scene_graph` and `scene_graph_revision`. Notifications are
`scene_graph_changed` and `scene_journal_gap`.

The source projection is produced only through Godot `PackedScene`,
`SceneState`, `Resource`, `Animation`, `ProjectSettings`, and `InputMap` APIs.
It contains bounded scene, node, serialized property, instance, connection,
group, subresource, animation-path, diagnostic, and allowlisted project-context
observations. The sidecar MUST NOT raw-parse `.tscn` as a second semantic
authority. Exact DTOs, limits, allowed settings, NodePath grammar, and negative
cases are frozen in `scene.schema.json` and `SCENE-001`.

Negotiation selects the highest supported minor no greater than 1.3. A 1.3
server accepts 1.0–1.2 clients without advertising or sending scene-domain
messages; resource-domain messages remain available to both 1.2 and 1.3
clients. Revision vectors omit `scene_graph_revision` below 1.3 and omit
`resource_revision` below 1.2.
