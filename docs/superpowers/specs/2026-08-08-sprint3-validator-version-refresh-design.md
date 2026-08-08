# Sprint 3 Validator Version Refresh Design

## Goal

Keep the Sprint 3 Rust storage validator reproducible while preventing a routine
`godot-codex-mcp` workspace version bump from invalidating its nested
`Cargo.lock`. The validator must continue to perform the canonical Rust scoring
check, must not modify the checkout, and must not change any Sprint 5 artifact or
test.

## Current failure

`tests/codex/storage_spike/Cargo.lock` records the path package
`godot-codex-index-store` at version `0.1.0`, while the current workspace package
version is `0.1.19`. Every Sprint 3 validation invokes Cargo with `--locked`, so
Cargo exits before the scoring code runs. `sprint3_acceptance.py` then replaces
that Cargo error with the misleading claim that the storage aggregate differs
from canonical scoring.

The storage-spike source also predates the required `script` field on
`IndexGeneration`. That compatibility error is independent from the version
refresh mechanism and must be updated explicitly.

## Chosen design

Add one Sprint 3-only Python launcher responsible for executing
`codex-storage-spike`:

1. Read the current workspace version from `godot-codex-mcp/Cargo.toml`.
2. Create a private temporary source tree containing the storage-spike crate and
   the compile-time Sprint 3 source-scope manifest.
3. Rewrite only the temporary dependency path so it addresses the current
   checkout's `godot-codex-index-store` crate.
4. Rewrite only the temporary lock entry for the path package
   `godot-codex-index-store` to the current workspace version.
5. Run Cargo with `--locked --offline` against that temporary manifest and a
   reusable ignored target directory.
6. Return the child process result without changing tracked files.

The launcher will reject ambiguous manifests, duplicate lock entries, malformed
versions, or any attempt to refresh a registry/git dependency. Automatic refresh
is deliberately limited to the version of the one known local path dependency;
all dependency-set changes remain reviewable failures.

`sprint3_acceptance.py`, the Sprint 3 aggregation runner, the Linux evidence
runner, and the storage-spike README will use this launcher. This prevents a
future workspace version bump from reintroducing the same failure through a
different Sprint 3 entry point.

## Error handling

The acceptance validator will distinguish three failure classes:

- launcher/preparation failure: the isolated validator could not be prepared;
- Cargo/build failure: the Rust validator could not run, with bounded stderr
  retained for diagnosis;
- canonical scoring failure: Cargo ran the validator and the evidence differed
  from the canonical raw-sample result.

The generic scoring message will no longer be used for every non-zero Cargo exit.
No network fallback is allowed.

## Compatibility update

The two Sprint 3 `IndexGeneration` constructors will initialize the current
`script` domain with its default empty generation, matching the existing scene
compatibility pattern. This is an explicit source compatibility update, not an
automatic schema migration.

## Tests

The change will be developed test-first and must prove:

- an intentionally stale local path-package version is refreshed in the
  temporary lock;
- the tracked `Cargo.toml` and `Cargo.lock` bytes remain unchanged;
- only the exact `godot-codex-index-store` path-package entry is eligible;
- malformed, duplicate, registry-backed, or unrelated lock drift fails closed;
- Cargo is invoked with `--locked --offline` against the temporary manifest;
- the existing valid storage evidence passes canonical Rust validation;
- tampered raw scoring remains rejected;
- all 28 Sprint 3 acceptance tests pass;
- Sprint 5 files remain untouched.

## Non-goals

- Repairing or regenerating Sprint 5 source-scope evidence.
- Automatically adapting storage-spike source code to future API/schema changes.
- Updating arbitrary Cargo dependencies during validation.
- Writing generated lock changes back into the repository.
