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
31. [SPRINT-5-EVIDENCE.md](SPRINT-5-EVIDENCE.md) — macOS live evidence and the explicit non-qualifying Windows waiver used to close Sprint 5.
32. [SPRINT-6-PLAN.md](SPRINT-6-PLAN.md) — executable find-usages, evidence aggregation, bounded context, and local acceptance plan.
33. [EVIDENCE-001-semantic-facts-and-evidence.md](EVIDENCE-001-semantic-facts-and-evidence.md) — canonical fact/evidence identity, deduplication, confidence, conflict, and safety rules.
34. [CONTEXT-001-model-facing-context.md](CONTEXT-001-model-facing-context.md) — unified find-usages and token-bounded MCP resource contract.
35. [SPRINT-6-EVIDENCE.md](SPRINT-6-EVIDENCE.md) — source-bound local macOS acceptance, live rename phases, SLOs, and Semantic Alpha / M1 closeout.
36. [SPRINT-7-PLAN.md](SPRINT-7-PLAN.md) — implementation-ready live editor overlay, history, MCP, and local macOS acceptance plan.
37. [EDITOR-001-live-editor-context.md](EDITOR-001-live-editor-context.md) — authoritative open-scene, Inspector, script-tab, native-history, revision, and live-overlay contract.
38. [SPRINT-7-EVIDENCE.md](SPRINT-7-EVIDENCE.md) — source-bound macOS live editor acceptance, lifecycle/revision conflicts, SLOs, and Live Editor Alpha closeout.
39. [SPRINT-8-PLAN.md](SPRINT-8-PLAN.md) — implemented EditorDebugger lifecycle, runtime observation/control, bounded screenshots, and recovery.
40. [RUNTIME-001-debugger-and-runtime-observation.md](RUNTIME-001-debugger-and-runtime-observation.md) — runtime session, remote tree, diagnostics, source mapping, and safety contract.
41. [SPRINT-9-PLAN.md](SPRINT-9-PLAN.md) — implemented host-approved editor-native transactions and targeted Undo.
42. [WRITE-001-editor-transactions-and-undo.md](WRITE-001-editor-transactions-and-undo.md) — transaction lifecycle, approval boundary, compound changes, persistence, and Undo contract.
43. [SPRINT-10-PLAN.md](SPRINT-10-PLAN.md) — completed compound changes, automatic validation/rollback, and M3 qualification.
44. [VALIDATION-001-automatic-validation-and-rollback.md](VALIDATION-001-automatic-validation-and-rollback.md) — validation report, rollback policy, and recovery contract.
45. [SPRINT-11-PLAN.md](SPRINT-11-PLAN.md) — external Codex productization, setup/doctor/offline/package/parity/usability plan.
46. [SPRINT-11-HOST-CONTRACT.md](SPRINT-11-HOST-CONTRACT.md) — exact local App/CLI/IDE/Godot candidate coordinate and open feasibility gates.
47. [EXTERNAL-CODEX-BETA-GUIDE.md](EXTERNAL-CODEX-BETA-GUIDE.md) — install/setup/status/read/runtime/write/offline/multi-project/troubleshooting workflow.
48. [TEST-001-fixtures-and-e2e-strategy.md](TEST-001-fixtures-and-e2e-strategy.md) — closed fixture, trace, usability, package, and source-bound evidence rules.

## Document status

| Document | Status | Next gate |
|---|---|---|
| `MASTER_SPRINT_ROADMAP` | Review candidate 1.0 | Updated from actual sprint evidence |
| `PRODUCT-001` | Approved 1.0 | Re-review on scope or invariant change |
| `ARCHITECTURE-001` | Draft for review 0.1 | `TEST-001` mapping and independent component/security review |
| `RELEASE-001` | Draft for review 0.1 | `TEST-001` evidence manifest |
| `ADR-000` | Accepted | Revisit only if fork topology changes |
| `ADR-001` | Accepted and locally verified | Production sidecar boundary starts in Sprint 2 |
| `PROTOCOL-001` | Bridge RPC 1.0–1.8 implemented and locally qualified through compound validation | Sprint 11 adds no Bridge semantics; retain lower-minor regressions |
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
| `INDEX-001` semantic index contract | Logical schema 1.3/`segment-v3`, independent resource/scene/script freshness, atomic composition, and bounded symbol queries pass the macOS live gate | Windows Sprint 5 qualification is waived/unverified |
| Sprint 4 plan | Complete; `S4-01`–`S4-10` and all 12 acceptance criteria pass on one macOS/Windows freeze | Begin Sprint 5 script and symbol intelligence |
| `SCENE-001` scene contract | D-06 implemented and live-verified through Bridge/MCP on macOS and Windows | Extend only through a versioned post-Sprint-4 contract |
| `SCRIPT-001` script contract | D-07 through strict Bridge RPC 1.4 normalization, `segment-v3`, atomic cross-domain composition, and both symbol tools pass on macOS arm64 | Windows parity remains waived/unverified |
| Sprint 5 plan | Closed with waiver; implementation and macOS live/accuracy/SLO gates pass, Windows and `S5-AC-12` remain unverified | Superseded by Sprint 6; retain the Windows release risk |
| Sprint 6 plan | Complete; `S6-01`–`S6-10` pass the source-bound local macOS gate | Begin Sprint 7 full live editor context |
| `EVIDENCE-001` / `CONTEXT-001` | Accepted and locally verified; 7/7 oracle facts, zero false `exact`, bounded resources | Extend only through a versioned post-Sprint-6 contract |
| Sprint 6 evidence | Final for the agreed macOS-only coordinate; Windows/Linux/remote CI are `not_run` | Semantic Alpha / M1 achieved; begin Sprint 7 |
| Sprint 7 plan | Complete; `S7-01`–`S7-10` pass the source-bound local macOS gate | Superseded by Sprint 8 |
| `EDITOR-001` live editor contract | Accepted and locally verified through Bridge RPC 1.5 | Retained unchanged through Sprints 8–11 |
| Sprint 7 evidence | Final for the agreed macOS-only coordinate; Windows/Linux/remote CI and model-facing smoke are `not_run` | Live Editor Alpha achieved |
| Sprint 8 plan / `RUNTIME-001` | Complete locally on macOS arm64 through Bridge RPC 1.6 | Runtime regressions remain blocking for Sprint 11 |
| Sprint 9 plan / WRITE-001 v1 | Complete locally through Bridge RPC 1.7 and targeted native Undo | Superseded by Sprint 10 compound profile |
| Sprint 10 plan / VALIDATION-001 / WRITE-001 v2 | Complete locally on source `b225f77acf48648ef6f59a76ff5a7dbb824da7bb`; evidence-only commit `815bccedddcd7c64a387dc8e9d5de2e941003fea` | M3 achieved; immutable Sprint 11 input |
| Sprint 11 plan / host contract / TEST-001 | In progress; local candidate and fail-closed external beta gates frozen | Implement and qualify App/CLI/official IDE package workflow |

## Interim ownership

Until roles are delegated to separate maintainers, fork maintainer `Sergan2B` is the interim Product, Engine/Editor, Sidecar/Protocol, Codex Client, QA, Security, Platform, and Release owner. An owner may delegate a role without changing product scope. Independent security and release review is still required by the later Alpha, Beta, RC, and Stable gates.

## Change rules

- Update document status and the affected acceptance mapping in the same change.
- Record cross-component or compatibility decisions in an ADR before implementation depends on them.
- Do not mark an implementation gate complete without the evidence required by the parent document.
- Use relative links inside this directory and keep generated build artifacts out of Git.
