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
14. [MCP-001-project-scoped-read-tools.md](MCP-001-project-scoped-read-tools.md) — Sprint 2 MCP contract for project-scoped read tools.
15. [SPRINT-2-USAGE.md](SPRINT-2-USAGE.md) — build, Codex configuration, smoke workflow, and verification commands.
16. [SPRINT-2-EVIDENCE.md](SPRINT-2-EVIDENCE.md) — implemented scope and reproducible Windows/macOS live verification.
17. [SPRINT-3-PLAN.md](SPRINT-3-PLAN.md) — implementation-ready ResourceUID, dependency graph, persistent index, and evidence plan.
18. [SPRINT-3-STAGE-1-PLAN.md](SPRINT-3-STAGE-1-PLAN.md) — executable `INDEX-001`, identity-rule, fixture, and golden-graph plan for `S3-01`/`S3-02`.
19. [SPRINT-3-STAGE-2-PLAN.md](SPRINT-3-STAGE-2-PLAN.md) — completed local SQLite-versus-segment spike, fault matrix, metrics, and `D-05` decision.
20. [SPRINT-3-STAGE-3-PLAN.md](SPRINT-3-STAGE-3-PLAN.md) — completed Bridge RPC 1.2, ResourceGraphAdapter, bounded journal, Rust wire-client, and local eight-phase evidence.
21. [INDEX-001-semantic-index-storage-and-migrations.md](INDEX-001-semantic-index-storage-and-migrations.md) — frozen resource identity/logical-index contract and local segment-store decision.
22. [SPRINT-3-STAGE-4-PLAN.md](SPRINT-3-STAGE-4-PLAN.md) — production resource index, ingestion/recovery, and two MCP resource tools.
23. [SPRINT-3-STAGE-5-PLAN.md](SPRINT-3-STAGE-5-PLAN.md) — completed Windows/macOS live gates, two-host storage matrix, SLOs, and final evidence aggregation.
24. [SPRINT-3-EVIDENCE.md](SPRINT-3-EVIDENCE.md) — final cross-platform evidence and requirement-by-requirement Sprint 3 acceptance.
25. [Sprint 3 host and aggregation runbook](../../tests/codex/runners/README.md) — fail-closed Windows reproduction and deterministic aggregation procedure.
26. [SPRINT-4-PLAN.md](SPRINT-4-PLAN.md) — implementation-ready scene semantics, Bridge RPC 1.3, persistent scene index, MCP, and local two-host gates.
27. [SPRINT-4-EVIDENCE.md](SPRINT-4-EVIDENCE.md) — local macOS/Windows scene gate, SLO policy, raw reports, and deterministic aggregate.
28. [SCENE-001-scene-node-resource-model.md](SCENE-001-scene-node-resource-model.md) — frozen D-06 scene/node/subresource identity and composition contract.
29. [SCRIPT-001-gdscript-and-csharp-adapters.md](SCRIPT-001-gdscript-and-csharp-adapters.md) — frozen D-07 analyzer authority, script identities, ranges, confidence, and adapter contract.
30. [SPRINT-5-PLAN.md](SPRINT-5-PLAN.md) — implementation-ready GDScript symbols, script index, MCP, and local two-host acceptance plan.

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
| Sprint 2 implementation | Windows x86_64 and macOS arm64 live editor→bridge→sidecar→MCP evidence complete | Optional external Codex UX capture; Sprint 3 implementation |
| Sprint 3 plan | Complete; `S3-01`–`S3-10` accepted locally | Begin Sprint 4 structural scene index |
| Sprint 3 Stage 1 | Complete and locally verified | Execute `S3-03` storage spike; freeze `D-05` by Sprint day 5 |
| Sprint 3 Stage 2 | Complete locally; full macOS matrix selected segment store | Begin `S3-04`; keep Windows/Linux portability verification separate and explicit |
| Sprint 3 Stage 3 | Complete and locally verified on macOS arm64 | Begin `S3-06` segment-store ingestion; Windows/Linux remain `not_run` |
| Sprint 3 Stage 4 | Complete and locally verified on macOS arm64 | Run `S3-09`; Windows/Linux and remote CI remain `not_run` |
| Sprint 3 Stage 5 | Complete on freeze `a90ddd06c81a`; macOS/Windows live and storage gates pass | Superseded by final Sprint 3 evidence |
| Sprint 3 evidence | Final; all 12 criteria pass and canonical D-05 selects segment | Begin Sprint 4 |
| `INDEX-001` resource contract | Frozen and implemented; cross-platform `D-05` selects segment store | Extend only through a versioned Sprint 4 contract |
| Sprint 4 plan | Complete; `S4-01`–`S4-10` and all 12 acceptance criteria pass on one macOS/Windows freeze | Begin Sprint 5 script and symbol intelligence |
| `SCENE-001` scene contract | D-06 implemented and live-verified through Bridge/MCP on macOS and Windows | Extend only through a versioned post-Sprint-4 contract |
| `SCRIPT-001` script contract | D-07 accepted; identities, ranges, confidence and enabled/disabled GDScript profiles locally verified on macOS arm64 | Build the independent `S5-02` oracle without production-derived expectations |
| Sprint 5 plan | In progress; `S5-01` complete locally, `S5-02`–`S5-10` remain | Execute `S5-02`: freeze the independent script fixture and golden oracle |

## Interim ownership

Until roles are delegated to separate maintainers, fork maintainer `Sergan2B` is the interim Product, Engine/Editor, Sidecar/Protocol, Codex Client, QA, Security, Platform, and Release owner. An owner may delegate a role without changing product scope. Independent security and release review is still required by the later Alpha, Beta, RC, and Stable gates.

## Change rules

- Update document status and the affected acceptance mapping in the same change.
- Record cross-component or compatibility decisions in an ADR before implementation depends on them.
- Do not mark an implementation gate complete without the evidence required by the parent document.
- Use relative links inside this directory and keep generated build artifacts out of Git.
