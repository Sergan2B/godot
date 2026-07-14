# ADR-001 — Component boundaries, Rust sidecar, and packaging

**Status:** Accepted

**Date:** 2026-07-14

**Decision owner:** `Sergan2B` (interim Engine/Editor and Sidecar/Protocol owner)

**Parent documents:** [MASTER_SPRINT_ROADMAP.md](MASTER_SPRINT_ROADMAP.md), Sprint 1; [PRODUCT-001-semantic-bridge-vision-and-plan.md](PRODUCT-001-semantic-bridge-vision-and-plan.md); [ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md](ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md)

## Context

The integration needs two very different execution environments:

- an editor-only component that can use Godot internal C++ APIs on the editor main thread;
- an isolated process that owns Bridge RPC, MCP lifecycle, semantic indexing, evidence aggregation, and future model-facing operations without extending the lifetime or failure domain of the editor.

The sidecar must be installable on macOS, Windows, and Linux without asking users to install a language runtime. The repository must also contain a protocol conformance client in Sprint 1 and later share the same Bridge RPC schemas with the production sidecar. Dependency resolution and release evidence must remain reproducible at a specific commit.

## Decision

### Component and process boundaries

`modules/codex_bridge` is an editor-only C++ module inside Godot. It is the only component allowed to include or call internal editor APIs such as `EditorNode`, `EditorData`, `EditorSelection`, `EditorDebugger`, or `EditorUndoRedoManager`.

The bridge module owns:

- editor-session lifecycle and the project-local endpoint;
- authentication, framing, bounded request dispatch, and cancellation;
- main-thread adapters and detached protocol DTOs;
- editor-native transactions and revision/event production in later sprints.

The bridge module does not own MCP, prompts, Codex authentication, an OpenAI SDK, a persistent semantic index, or client-specific UI text.

`godot-codex-mcp` is a separate local process. It owns the Bridge RPC client, the future semantic index and evidence/query layers, stdio MCP, offline/read-only behavior, and approval routing. It does not include Godot private headers, receive editor pointers or `ObjectID` values outside an explicitly session-scoped opaque DTO, or create a raw-file write path for an open scene.

The Godot Dock later uses Codex app-server for Codex lifecycle and the same project-scoped MCP sidecar for Godot semantics. It does not become a third semantic implementation.

### Sidecar language and toolchain

The production sidecar and the Sprint 1 conformance client are implemented in Rust.

- The baseline compiler is Rust `1.94.1`, a stable release in the established local Sprint 1 environment.
- A committed `rust-toolchain.toml` must pin an exact stable toolchain when the first Rust workspace is scaffolded. A toolchain update is an explicit reviewed change with a compatibility run.
- Nightly-only language features and build steps are not allowed in release artifacts.
- `unsafe` is denied by default in project crates. A platform adapter may use a narrowly scoped `unsafe` block only with a stated invariant and a targeted test.

Rust is selected for a small native deployment artifact, explicit error handling, memory-safe concurrent code, and mature cross-platform process and local-transport support. This decision does not move Godot editor access out of C++.

### Dependency policy

- Every deployable Rust workspace commits `Cargo.lock`.
- CI and release builds use `cargo build --locked`; they never update dependency resolution implicitly.
- Default features are disabled when they pull in unused network, TLS, async-runtime, or serialization surfaces.
- New direct dependencies require a purpose, license review, maintenance assessment, and evidence that the standard library or an existing dependency is insufficient.
- Git dependencies, unpinned path overrides outside this repository, build-time downloads, and dependencies that require an installed language runtime are prohibited in release builds.
- Release evidence records the Rust toolchain, target triple, lockfile hash, dependency/license manifest, and final artifact hash.
- Vendoring is optional for ordinary development and mandatory only if the release/reproducibility process later proves that an offline source bundle is required.

The conformance client may use test-only dependencies, but they remain locked and must not enter the production sidecar dependency graph accidentally.

### Packaging

Each supported target produces one native executable named `godot-codex-mcp` plus separately versioned documentation/configuration assets. The executable may use operating-system libraries that are part of the supported platform, but it must not require an installed Rust toolchain, Cargo, interpreter, VM, package manager, or background service.

The sidecar is launched per canonical project root as a stdio MCP server. It is not installed as a global daemon and does not enumerate other open projects. Process discovery remains project-local through `.godot/codex/`.

Debug symbols, notices, SBOM/provenance, and signatures are release artifacts, not runtime prerequisites for starting the single executable.

### Repository topology

The future production sidecar stays in this monorepo so protocol, engine adapter, tests, and compatibility evidence can change atomically:

```text
godot-codex-mcp/
├── Cargo.toml
├── Cargo.lock
├── rust-toolchain.toml
└── crates/
    ├── bridge-client/
    ├── semantic-model/
    ├── index/
    ├── mcp-server/
    ├── transactions/
    └── godot-codex-mcp/

schemas/codex_bridge/          # canonical wire schemas and fixtures
tests/codex/                   # conformance client and Godot fixtures
modules/codex_bridge/          # editor-only C++ implementation
```

Sprint 1 creates only the Rust conformance client under `tests/codex`; production sidecar behavior begins in Sprint 2. Shared schemas are read from `schemas/codex_bridge/` and are not copied into C++ and Rust source trees by hand. Generated bindings, if introduced, must be reproducible from those schemas and checked for drift.

The sidecar remains in the Godot fork repository through 1.0. Moving it to a separate repository requires a new ADR that preserves atomic protocol compatibility evidence and release provenance.

### Version boundaries

- The bridge module and sidecar use independent Semantic Versions, starting at `0.1.0` when their artifacts exist.
- Bridge RPC wire compatibility is independent of component Semantic Versions and starts at `1.0` in [PROTOCOL-001-bridge-rpc-v1.md](PROTOCOL-001-bridge-rpc-v1.md).
- The Rust crate graph may evolve internally without a protocol change; a wire-incompatible change requires a Bridge RPC major version and migration/conformance fixtures.
- The canonical schema bundle is selected by Bridge RPC major version, not by the Rust crate version.

## Consequences

- Editor crashes, sidecar crashes, MCP restarts, and index rebuilds have separate failure domains.
- C++ remains necessary at the Godot boundary, while network-shaped parsing, concurrency, persistence, and MCP code remain outside the editor process.
- The repository gains a second build toolchain and must maintain Rust dependency, license, and artifact evidence in addition to SCons/C++ evidence.
- A single executable simplifies setup and doctor behavior, but each supported OS/architecture still needs its own built, tested, and signed artifact.
- Keeping sidecar and engine code in one repository increases repository scope but makes protocol changes and their conformance evidence reviewable in one commit.

## Rejected alternatives

- **C++ sidecar:** shares language with Godot but expands memory-safety and concurrency risk in protocol/index/MCP code and complicates a small cross-platform CLI artifact.
- **Python or Node.js sidecar:** speeds early prototyping but requires either an installed runtime or a larger runtime bundle and weakens the single-native-binary constraint.
- **Embedding MCP and the index in Godot:** puts unbounded parsing, storage, and client lifecycle in the editor failure domain and violates the main-thread/performance boundary.
- **Running the bridge as a GDExtension or editor plugin:** cannot provide the same controlled access to the internal editor APIs required by the roadmap and would create another compatibility surface.
- **Separate sidecar repository before 1.0:** makes atomic schema/adapter changes, bisecting, and commit-bound release evidence harder without providing a current product benefit.
- **Rust nightly:** adds avoidable compiler churn and undermines the reproducible stable-toolchain policy.
- **Unlocked or floating dependencies:** make the same source commit resolve to different production code and invalidate artifact evidence.

## Acceptance checks

- No sidecar or conformance source includes Godot private editor headers.
- `modules/codex_bridge` has no MCP, OpenAI, Codex-account, or persistent-index dependency.
- The first Rust workspaces commit exact toolchain and lock files and build with `--locked`.
- A release candidate starts from its native executable on a clean target OS without Rust/Cargo or another language runtime installed.
- Bridge RPC schemas have one canonical repository location and a drift check once generated bindings exist.
- Component and wire versions are recorded independently in evidence.

## Follow-up

- Sprint 1: scaffold the editor-only module and `tests/codex` conformance client against the canonical v1 schemas.
- Sprint 2: create the production Rust workspace and the first BridgeClient/MCP vertical slice.
- Sprint 14–17: finalize dependency audit, SBOM/provenance, reproducible packaging, signing, and three-platform release evidence.
