# Godot × Codex integration documents

This directory is the authoritative entry point for the Godot × Codex integration. Product scope is defined before implementation details; implementation documents may refine a contract but must not silently weaken a parent invariant.

## Reading order

1. [MASTER_SPRINT_ROADMAP.md](MASTER_SPRINT_ROADMAP.md) — scope, milestones, sprint order, and release gates.
2. [PRODUCT-001-semantic-bridge-vision-and-plan.md](PRODUCT-001-semantic-bridge-vision-and-plan.md) — product invariants and semantic model.
3. [ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md](ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md) — component boundaries and foundation backlog.
4. [RELEASE-001-release-1.0-acceptance-and-evidence-plan.md](RELEASE-001-release-1.0-acceptance-and-evidence-plan.md) — release requirements and evidence rules.
5. [ADR-000-fork-and-upstream-strategy.md](ADR-000-fork-and-upstream-strategy.md) — fork maintenance, branching, and versioning policy.
6. [ADR-001-component-boundaries-and-sidecar-language.md](ADR-001-component-boundaries-and-sidecar-language.md) — C++/Rust boundaries, dependency policy, packaging, and monorepo topology.
7. [PROTOCOL-001-bridge-rpc-v1.md](PROTOCOL-001-bridge-rpc-v1.md) — Bridge RPC 1.0 framing, discovery, authentication, lifecycle, and compatibility.
8. [BUILDING_CODEX_FORK.md](../../BUILDING_CODEX_FORK.md) — reproducible local macOS build and verification.
9. [SPRINT-0-BASELINE.md](SPRINT-0-BASELINE.md) — commit-bound local build and test evidence.
10. [SPRINT-1-STAGE-2.md](SPRINT-1-STAGE-2.md) — editor-only module, lifecycle, dispatcher, and build-guard evidence.
11. [SPRINT-1-STAGE-3.md](SPRINT-1-STAGE-3.md) — private discovery, authenticated macOS UDS, framing, and handshake evidence.
12. [SPRINT-1-STAGE-4.md](SPRINT-1-STAGE-4.md) — authenticated RPC lifecycle, deadlines, cancellation, and backpressure evidence.
13. [SPRINT-1-STAGE-5.md](SPRINT-1-STAGE-5.md) — Rust conformance client, canonical redacted trace, and final local Sprint 1 evidence.

## Document status

| Document | Status | Next gate |
|---|---|---|
| `MASTER_SPRINT_ROADMAP` | Review candidate 1.0 | Updated from actual sprint evidence |
| `PRODUCT-001` | Approved 1.0 | Re-review on scope or invariant change |
| `ARCHITECTURE-001` | Draft for review 0.1 | `TEST-001` mapping and independent component/security review |
| `RELEASE-001` | Draft for review 0.1 | `TEST-001` evidence manifest |
| `ADR-000` | Accepted | Revisit only if fork topology changes |
| `ADR-001` | Accepted and locally verified | Production sidecar boundary starts in Sprint 2 |
| `PROTOCOL-001` | Accepted and locally verified for Sprint 1 | Remote CI and independent review |
| Sprint 0 local baseline | Passed | Remote CI is `not_run` until publication is authorized |
| Sprint 1 Stage 2 local evidence | Passed | Superseded by Stage 3 evidence |
| Sprint 1 Stage 3 local evidence | Passed | Superseded by Stage 4 evidence |
| Sprint 1 Stage 4 local evidence | Passed | Superseded by Stage 5 evidence |
| Sprint 1 Stage 5 local evidence | Passed and committed locally | Separately authorized publication and remote CI |

## Interim ownership

Until roles are delegated to separate maintainers, fork maintainer `Sergan2B` is the interim Product, Engine/Editor, Sidecar/Protocol, Codex Client, QA, Security, Platform, and Release owner. An owner may delegate a role without changing product scope. Independent security and release review is still required by the later Alpha, Beta, RC, and Stable gates.

## Change rules

- Update document status and the affected acceptance mapping in the same change.
- Record cross-component or compatibility decisions in an ADR before implementation depends on them.
- Do not mark an implementation gate complete without the evidence required by the parent document.
- Use relative links inside this directory and keep generated build artifacts out of Git.
