# ADR-000 — Fork and upstream strategy

**Status:** Accepted

**Date:** 2026-07-14

**Decision owner:** `Sergan2B` (interim fork maintainer)

**Base repository:** `Sergan2B/godot`

**Initial upstream base:** `2c089e9bf0b8712d0bc444c2ceaf9c543ed9c777`

## Context

The Codex integration is a long-lived Godot fork that depends on internal editor APIs. Development directly on a moving upstream branch would mix integration regressions with upstream changes and make release evidence difficult to reproduce. The fork therefore needs an explicit base commit, auditable upstream updates, and a branch that can carry Codex work independently of the selected Godot baseline.

## Decision

### Repository and branch topology

- `upstream` fetches from `https://github.com/godotengine/godot.git`; it is not a push destination.
- `origin` is the `Sergan2B/godot` fork.
- `master` represents the selected upstream baseline. It advances only during an explicit upstream-sync change.
- `codex/integration` is the long-lived integration branch.
- Feature branches use `codex/<area>-<short-name>` and merge into `codex/integration`.
- Release stabilization uses `codex/release-<version>`; release branches do not replace the integration branch.

The initial integration commit is `b66a148e3f784eed08a125a31f7b45a1bd3dfb2e`, whose parent is the selected upstream base.

### History policy

Maintainer clones keep complete commit and tree history so merge-base calculation, blame, log, bisect, and upstream merges remain reliable. Blobless partial storage (`--filter=blob:none`) is allowed: missing historical blobs are fetched on demand and do not make the repository shallow. Shallow maintainer clones are not supported for an upstream-sync operation.

Published `codex/integration` history is never rebased. Upstream changes enter through an explicit merge commit, preserving the chosen upstream commit and the conflict resolution performed for that sync.

### Upstream sync procedure

1. Start with a clean worktree and fetch `upstream/master` with complete commit history.
2. Select and record a concrete upstream SHA; never sync to an unnamed moving tip.
3. Fast-forward local `master` to that SHA after reviewing upstream build and compatibility changes.
4. Create `codex/upstream-<YYYYMMDD>` from `codex/integration`.
5. Merge `master` with `--no-ff`, resolve conflicts on the sync branch, and run the Build Baseline plus all integration gates available at that sprint.
6. Merge the verified sync branch into `codex/integration` and update the base commit, compatibility notes, and evidence.

If a sync fails after publication, revert the sync merge. Do not rewrite the integration branch. If it has not been published, the sync branch may be discarded while `master` remains at the selected baseline.

### Versioning policy

- The engine keeps the upstream Godot version (`4.8-dev` at the initial base). Codex builds use `BUILD_NAME=codex`; the exact Git SHA is part of every evidence record.
- The bridge module and sidecar have independent Semantic Versions, beginning at `0.1.0` when their artifacts first exist.
- Bridge RPC and MCP schemas use independent `major.minor` versions. Bridge RPC starts at `1.0` because Sprint 1 establishes its first wire-compatibility boundary; every breaking Bridge RPC change increments the major version. MCP starts at `0.1`; before MCP 1.0, a breaking change increments its minor version and includes a migration note, and at/after 1.0 it increments the major version.
- Index and transaction formats declare their own schema versions and migration or rebuild behavior.
- Compatibility evidence records the Godot SHA, bridge version, sidecar version, Bridge RPC version, MCP schema version, index version, fixture revision, platform, and artifact hash. A component that does not yet exist is recorded as `not_applicable`, not assigned a fictional version.

## Consequences

- Upstream updates are deliberate and may lag upstream `master` while a release gate is being stabilized.
- Sync merge commits make the history noisier but provide an auditable compatibility boundary.
- Full commit history costs more initial network time; blobless storage limits that cost without weakening history operations.
- Codex-specific changes remain isolated from the branch representing the selected Godot base.

## Rejected alternatives

- **Develop directly on `master`:** makes the upstream baseline and integration state indistinguishable.
- **Regularly rebase `codex/integration`:** rewrites shared history and invalidates commit-bound evidence.
- **Permanent shallow clone:** cannot reliably support arbitrary merge-base, bisect, or historical inspection.
- **Merge every upstream tip automatically:** introduces unreviewed internal API drift and defeats the pinned-base gate.

## Acceptance checks

- `master` points at the selected base and `codex/integration` contains Codex work.
- `git rev-parse --is-shallow-repository` returns `false`.
- `git remote get-url upstream` returns the official Godot URL.
- The Build Baseline passes before bridge implementation and after every upstream sync.
