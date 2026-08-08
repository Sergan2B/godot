# Sprint 11.5 Architecture Stabilization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Выполнить behavior-preserving стабилизацию архитектуры Rust sidecar и C++ Godot Bridge и выпустить полностью квалифицированный пакет `godot-codex 0.1.20` без изменения внешних контрактов.

**Architecture:** Работа идёт вертикальными risk-first волнами. Dependency direction фиксируется как `presentation -> application -> domain/ports`, concrete adapters зависят от inward-owned ports, а composition roots — единственные места, которым разрешено знать все реализации. Каждый extraction сначала защищается characterization/contract-тестом, затем выполняется без смешивания с изменением поведения.

**Tech Stack:** Rust 1.94.1, Cargo workspace, Tokio, RMCP, Serde, C++17 Godot module, SCons, Godot doctest suite, Python 3.14 standard library, JSON Schema, macOS arm64 qualification tooling.

## Global Constraints

- Scope production-контура: Rust sidecar и C++ Godot Bridge.
- Внешние MCP, Bridge RPC `1.0`-`1.8`, CLI/config, index/storage, journal, setup receipt и evidence-контракты не меняются.
- Единственное допустимое observable изменение — версия package `0.1.20` и связанные manifest-bound coordinates.
- Внутренние Rust API, C++ классы, модули и crate-границы можно менять.
- Новых функций нет; Godot Dock и Sprint 12 capabilities не входят в Sprint 11.5.
- `vendor/rmcp`, общий рефакторинг Python harnesses, Sprint 5 Windows и human usability Sprint 11 не входят в scope.
- Узкие изменения Python qualification tooling разрешены только для manifest-bound `ReleaseCoordinate`, architecture/performance gates и выпуска `0.1.20`; это явное исключение из запрета общего рефакторинга harnesses.
- Максимум `1200` logical production LOC на hand-written `.rs`, `.cpp` или `.h` к финальному gate.
- Каждая волна выполняется в отдельной ветке/worktree от актуального `codex/integration` и сливается только после зелёного regression gate.
- Move/extract и behavior change нельзя смешивать в одном коммите.
- Исторические `v0119` и более ранние evidence artifacts immutable.
- Sprint 5 Windows остаётся `closed_with_waiver`; usability остаётся `deferred_unacquired`.
- Таймбокса нет: Sprint завершается только по gates этого документа.

---

## 1. Frozen behavior и граница scope

До первого extraction сохранить normalized golden traces для:

- connection states `ready`, `syncing`, `offline_empty`, `offline_cached`, `project_session_busy`, `misconfigured`;
- Bridge discovery, authentication, reconnect и same-project takeover;
- static resource/scene/script queries и evidence;
- live scene, selection, unsaved overlay и revision handling;
- двух последовательных runtime sessions, diagnostics, stack и screenshot;
- transaction prepare/accept/decline/cancel/timeout/apply/status/validation/rollback/Undo;
- setup preview/apply/repair, doctor fault matrix и capture lease lifecycle.

Публичные JSON поля, MCP registry, resource URIs, error codes, remediation IDs, ordering, pagination, digest/revision semantics и approval expiry сравниваются до и после каждой волны. Session IDs, timestamps, temporary roots и cryptographic nonces нормализуются по уже принятой Sprint 11 policy.

### Scope-creep rule

Найденная проблема исправляется внутри Sprint 11.5 только если она:

1. нарушает correctness или security;
2. блокирует текущий extraction либо делает characterization недостоверной;
3. воспроизводится bounded regression-тестом;
4. исправляется отдельным behavior-change commit до возобновления extraction.

Любая иная ошибка, оптимизация или feature request записывается в backlog с severity, reproduction и evidence, но не реализуется в Sprint 11.5. Рефакторинг нельзя расширять под предлогом «раз уж файл открыт».

---

## 2. Dependency direction и ownership

### 2.1 Rust

Разрешённое направление:

```text
presentation (mcp-server)
            |
            v
application (resource-indexer, transactions, operations services)
            |
            v
domain/contracts + inward-owned ports (product-core, semantic-model, ports)

adapters (bridge-client, index-store, surface-capture, fs/process implementations)
            |
            +-----------------------> domain/contracts + ports

composition roots (godot-codex-mcp, godot-codex)
            +-----------------------> application + adapters
```

Стрелка означает compile-time dependency от внешнего слоя к внутреннему. Вызов во время исполнения может идти через port в обратную сторону, но concrete adapter type при этом остаётся неизвестен application-коду.

Правила:

- Application crates не зависят от concrete adapter crates.
- Port принадлежит потребителю use case, а не adapter-реализации.
- DTO, которые пересекают port, принадлежат `product-core`, `semantic-model` или inward contracts; adapter-native handles не проходят внутрь.
- `mcp-server` зависит от application services и stable contracts, но не выполняет filesystem/process I/O, index writes или transaction algorithms.
- `godot-codex-mcp` и `godot-codex` создают implementations, связывают ports, запускают supervisors и выполняют shutdown; domain state machines в composition roots запрещены.
- `GodotMcpServer` остаётся публичным facade и хранит агрегат сервисов `connection`, `static_semantics`, `live_editor`, `runtime`, `transactions`.

Для устранения меж-crate обратных зависимостей допускается один dependency-light crate `godot-codex-ports`, если ADR-002 докажет compile-time isolation. Он содержит только capability-scoped traits и inward DTO references, не orchestration и не concrete I/O.

Любой последующий новый crate требует отдельной ADR-записи с:

- запрещаемым dependency edge;
- причиной, почему module visibility недостаточна;
- владельцем и потребителями API;
- compile-time либо trust-boundary выгодой;
- альтернативой «оставить module в существующем crate»;
- правилом удаления/слияния crate, если обоснование перестанет действовать.

Architecture checker не принимает решение о создании crate: он только проверяет уже утверждённую ADR policy.

### 2.2 C++ Bridge

- `CodexBridgeService` остаётся EditorPlugin lifecycle/composition root: signals, startup/shutdown и wiring.
- Request routing, snapshot coordination и event publication становятся отдельными компонентами.
- Runtime разделяется на lifecycle, tree/object projection, diagnostics/stacks и screenshots.
- Transport разделяется на private discovery/filesystem, listener lifecycle и wire worker.
- Transaction coordinator разделяется на state machine, apply scheduler и reconciler; существующие planners/executors сохраняются.
- Resource/scene/script adapters разделяются на Godot capture, bounded projection и delta production.
- Editor adapters возвращают bounded DTO/completions и не включают transport headers.
- Protocol не включает editor/transport; transport не включает editor.
- Единственный Godot singleton допускается как EditorPlugin lifecycle entry point, но не как service locator.

Поток данных:

```text
wire decode -> typed command -> main-thread dispatcher -> use case/adapter
            -> bounded DTO -> wire encode
```

Typed errors преобразуются в стабильные public error codes только на MCP/RPC boundary.

---

## 3. Architecture gates

### 3.1 Точный алгоритм LOC

Checker сканирует:

- `godot-codex-mcp/crates/*/src/**/*.rs`;
- `modules/codex_bridge/**/*.cpp`;
- `modules/codex_bridge/**/*.h`.

Не сканируются:

- любой path component `test` или `tests`;
- generated files, у которых первая non-blank строка равна `// @generated` и source generator указан в policy;
- declarative schema/table exception, записанный в policy с `path`, `owner`, `reason`, `kind = "declarative"` и `expires_after`.

`logical production LOC` — число физических строк, содержащих хотя бы один language token после лексического исключения whitespace, `//` comments и `/* ... */` comments. Лексер обязан учитывать quoted strings, Rust raw strings, character literals и escaped delimiters, поэтому comment markers внутри literal считаются кодом. Attributes, braces, preprocessor directives и строки multiline literal считаются. Несколько statements на одной строке считаются одной LOC и отдельно запрещаются formatter/linter gates.

На baseline checker записывает существующие violations как ratchet с точным `path`, `logical_loc` и file digest. После этого:

- новый oversized file запрещён;
- рост `logical_loc` существующего violation запрещён;
- изменение digest без уменьшения violation запрещено;
- удалённая ratchet entry не может вернуться;
- к closeout все non-declarative entries удалены, и каждый production file имеет не более `1200` logical LOC.

Inline test bodies должны быть вынесены в test modules/directories; они не используются для искусственного прохождения production gate.

### 3.2 Dependency и responsibility gates

- Architecture policy хранит crate categories, allowed Cargo edges, forbidden Rust imports и forbidden C++ include directions.
- Negative fixtures обязаны доказывать rejection для forbidden Cargo edge, forbidden include, oversized file, рост ratchet violation, просроченного exception и invalid generated marker.
- `serde_json::Value` и Godot `Variant` разрешены только на wire/adapter boundary или внутри bounded typed wrapper.
- Central facades не содержат domain state machine, storage mutation или response-building algorithms.
- Composition roots не содержат business branching, кроме startup/retry/shutdown policy.

### 3.3 Performance non-regression

До extraction записывается immutable baseline на чистом release build и неизменных fixtures. Каждый metric собирается после одного warm-up в пяти изолированных runs; evidence хранит все samples, run-level p95 и median run-level p95 вместе с machine/build coordinates.

Gate для каждого existing SLO metric:

- published absolute SLO из Sprint 6-11 остаётся обязательным;
- candidate median p95 не может быть выше baseline median p95 более чем на `10%`;
- status/ping during indexing остаётся `<= 200 ms` p95;
- cached semantic query остаётся `<= 300 ms` p95;
- current scene/selection остаётся `<= 500 ms` p95;
- incremental visibility и runtime transition остаются `<= 2 s` p95;
- offline cache activation остаётся `<= 2 s` p95;
- reconnect-to-ready остаётся `<= 5 s` p95;
- doctor остаётся `<= 3 s` offline и `<= 10 s` с Bridge;
- cached runtime page/diagnostic остаётся `<= 500 ms` p95;
- viewport capture остаётся `<= 3 s` p95.

Для metrics без published absolute SLO, steady-state RSS, двух release binaries и package archive действует relative ceiling `baseline * 1.10`. Превышение допускается только отдельной ADR/performance exception с измерениями, причиной, owner и expiry; архитектурный рефакторинг сам по себе не является достаточной причиной.

Baseline и candidate измеряются на одном host, OS, architecture, power mode, fixture set и build profile. Run с thermal throttling, background update или mismatched coordinate помечается invalid и повторяется, а не удаляется из evidence без записи причины.

---

## 4. Target file map

### New policy and qualification files

- `docs/codex-integration/ADR-002-sprint-11-5-production-boundaries.md` — dependency direction, port ownership и обоснование каждого нового crate.
- `tests/codex/architecture/production-boundaries.toml` — machine-readable layers, allowed edges, LOC policy, ratchet и exceptions.
- `tests/codex/architecture_guard.py` — deterministic Cargo/include/LOC checker.
- `tests/codex/test_architecture_guard.py` — positive и negative checker fixtures.
- `tests/codex/sprint115_performance.py` — bounded baseline/candidate runner и comparator.
- `tests/codex/test_sprint115_performance.py` — statistical policy, coordinate и regression tests.
- `tests/codex/schemas/sprint11-5-architecture-baseline.schema.json` — architecture/performance evidence schema.
- `tests/codex/sprint11_release_coordinate.py` — manifest-bound `ReleaseCoordinate` loader/validator.
- `tests/codex/test_sprint11_release_coordinate.py` — version/path/digest mismatch tests.

### Rust target modules

- `godot-codex-mcp/crates/ports/src/{lib,bridge,index,transactions}.rs` — inward-owned capability ports, если ADR-002 утверждает crate.
- `godot-codex-mcp/crates/godot-codex-mcp/src/{main,cli,supervisor,shutdown,capture}.rs` — sidecar composition root.
- `godot-codex-mcp/crates/mcp-server/src/services/{mod,connection,static_semantics,live_editor,runtime,transactions}.rs` — capability handles/facade consumed by MCP handlers; use-case algorithms остаются в application crates.
- `godot-codex-mcp/crates/mcp-server/src/tools/{mod,connection,static_semantics,live_editor,runtime,transactions}.rs` — validation/dispatch/output mapping only.
- `godot-codex-mcp/crates/mcp-server/src/output_schema/{mod,registry,availability,connection,static_semantics,live_editor,runtime,transactions}.rs` — exact existing public schemas split by capability.
- `godot-codex-mcp/crates/resource-indexer/src/{coordinator,ingestion,generation,queries}.rs` — semantic application orchestration.
- `godot-codex-mcp/crates/index-store/src/{lib,generation,records,query,segment}.rs` — persistence adapter.
- `godot-codex-mcp/crates/transactions/src/{lib,prepare,approval,apply,status,recovery,validation}.rs` — transaction use cases/state machines.
- `godot-codex-mcp/crates/operations/src/setup/{mod,plan,config,receipt,apply,validation}.rs` — setup policy and adapters.
- `godot-codex-mcp/crates/operations/src/doctor/{mod,checks,diagnostics,remediation,render}.rs` — doctor evaluation/rendering.
- `godot-codex-mcp/crates/surface-capture/src/lease/{mod,model,store,transition,validation}.rs` — lease state and persistence.

### C++ target modules

Новые C++ files остаются непосредственно в существующих `editor/`, `protocol/` и `transport/`, потому что `modules/codex_bridge/SCsub` уже glob-ит только эти каталоги.

- `editor/bridge_request_router.{h,cpp}` — typed command routing.
- `editor/snapshot_coordinator.{h,cpp}` — snapshot queue, deadlines и completion.
- `editor/bridge_event_publisher.{h,cpp}` — bounded event projection/publication.
- `editor/runtime_lifecycle_controller.{h,cpp}` — run/stop/session state.
- `editor/runtime_projection.{h,cpp}` — remote tree/object DTO projection.
- `editor/runtime_diagnostic_store.{h,cpp}` — bounded diagnostics/stacks.
- `editor/runtime_screenshot_service.{h,cpp}` — viewport policy/completion.
- `transport/bridge_discovery_store.{h,cpp}` — private discovery/auth material.
- `transport/bridge_listener.{h,cpp}` — listener lifecycle.
- `editor/transaction_state_machine.{h,cpp}` — canonical transaction transitions.
- `editor/transaction_apply_scheduler.{h,cpp}` — main-thread apply queue.
- `editor/transaction_reconciler.{h,cpp}` — lost response/status/rollback reconciliation.
- Existing resource/scene/script adapters split into capture/projection/delta units in `editor/` with capability-prefixed filenames.

---

## 5. Implementation waves

### Task 1: Freeze baseline, ADR and executable architecture policy

**Files:**
- Create all policy, guard, performance and ADR files listed in §4.
- Modify: `godot-codex-mcp/Cargo.toml` only if ADR-002 approves `crates/ports`.
- Test: `tests/codex/test_architecture_guard.py`, `tests/codex/test_sprint115_performance.py`.

**Interfaces:**
- Produces `ArchitecturePolicy.load(Path)`, `scan_repository(policy) -> ArchitectureReport`, `compare_performance(baseline, candidate) -> PerformanceReport` and immutable baseline evidence.
- Produces one closed Cargo layer matrix consumed by every later task.

- [ ] Write negative tests covering all LOC/dependency/exception failures in §3.
- [ ] Run `python3 -m unittest tests/codex/test_architecture_guard.py tests/codex/test_sprint115_performance.py` and verify RED because the modules do not exist.
- [ ] Implement the lexical LOC scanner, Cargo graph reader, include scanner, ratchet validation, coordinate validation and bounded comparator exactly as specified in §3.
- [ ] Record normalized behavior traces, file sizes, Cargo graph, include graph, performance samples, binary sizes and RSS on the pre-refactor commit.
- [ ] Run the unit tests again and require PASS; run the checker once in ratchet mode and once in strict-no-new-violation mode.
- [ ] Commit policy/tests separately from generated baseline evidence; evidence commit must not contain production changes.

### Task 2: Extract lifecycle and connection path

**Files:**
- Modify Rust composition-root files and create modules listed in §4.
- Modify `modules/codex_bridge/editor/codex_bridge_service.{h,cpp}` and `modules/codex_bridge/transport/bridge_runtime.{h,cpp}`.
- Create request router, snapshot, event, discovery and listener C++ modules from §4.
- Test: `godot-codex-mcp/crates/godot-codex-mcp/tests/offline_subprocess.rs`, `tests/codex/test_codex_bridge.cpp`, Sprint 11 same-project/process-scope tests.

**Interfaces:**
- `CodexBridgeService` owns lifecycle only.
- `BridgeRequestRouter` consumes typed commands and delegates on the main thread.
- `SnapshotCoordinator` owns request IDs, deadlines, cancellation and bounded completions.
- Rust supervisors expose start, health and idempotent shutdown without MCP response mapping.

- [ ] Add characterization cases for ready/syncing/offline/misconfigured/busy, stale discovery, authentication failure, reconnect and takeover.
- [ ] Run targeted Rust/C++/Python cases and verify at least one new responsibility test fails against the monolith.
- [ ] Extract one component per commit, preserving normalized RPC frames and public status projection.
- [ ] Run `cargo test --workspace --all-targets` and the Godot `*CodexBridge*` tests after each extraction.
- [ ] Compare normalized traces and Task 1 performance metrics; reject semantic drift or regression.

### Task 3: Extract static and live semantic capabilities

**Files:**
- Modify/split `resource-indexer`, `index-store`, `bridge-client` and semantic MCP modules from §4.
- Modify/split C++ resource, scene and script adapters and their delta journals.
- Test: existing resource/scene/script contract, fixture and live suites under `tests/codex/`.

**Interfaces:**
- Application owns ingestion/generation/query ports; concrete Bridge and index implementations are injected by the composition root.
- Domain/application paths exchange typed semantic DTOs; raw JSON/Variant remains at boundary projection.
- Generation activation stays atomic and preserves index schema `1.3` and existing segment format.

- [ ] Add failing dependency tests proving application cannot import bridge-client/index-store implementations.
- [ ] Add characterization tests for saved queries, evidence bounds, unsaved overlay, deltas and revisions.
- [ ] Move DTO ownership inward, introduce injected ports, then split orchestration/query/persistence modules without changing storage formats.
- [ ] Run resource/scene/script Rust, C++ and Python suites plus architecture guard.
- [ ] Compare traces, cache activation/reconnect metrics and index bytes for identical fixtures.

### Task 4: Thin MCP facade and extract runtime capability

**Files:**
- Split `mcp-server/src/lib.rs` and `output_schema.rs` into target modules from §4.
- Split `bridge-client/src/runtime.rs` and `runtime_debugger_adapter.cpp` into runtime components from §4.
- Test: exact registry/output-schema tests, Sprint 8 runtime suites and two-session live fixture.

**Interfaces:**
- MCP tools perform input validation, application call and stable output mapping only.
- Runtime service owns lifecycle/tree/diagnostic/screenshot use cases; C++ adapters return bounded typed completions.
- Registry remains exactly the frozen Sprint 11 public surface.

- [ ] Add failing tests that reject filesystem/process/index mutation imports from MCP presentation modules.
- [ ] Add characterization for two distinct runtime sessions, stop/restart, diagnostics stack and screenshot.
- [ ] Extract services, tools and schemas capability-by-capability; keep each move/extract commit behavior-neutral.
- [ ] Run registry/schema, Sprint 8 runtime, C++ runtime and architecture tests.
- [ ] Enforce runtime page/diagnostic, transition and screenshot performance gates.

### Task 5: Extract transaction state machines on both sides

**Files:**
- Split Rust `transactions/src/lib.rs` into target modules from §4.
- Split C++ `transaction_coordinator.{h,cpp}` into state machine, scheduler and reconciler.
- Preserve existing planners, stores, executors, canonicalizers and approval verifier.
- Test: Sprint 9/10 transaction suites and C++ transaction tests under `tests/codex/`.

**Interfaces:**
- Prepare produces the same canonical digest-bound preview.
- Approval/apply consumes unchanged digest, revision, sequence, expiry and idempotency coordinates.
- Reconciler owns lost-response, stale baseline, crash journal, validation/rollback and native Undo recovery.

- [ ] Add transition-table tests for accept/decline/cancel/timeout, idempotent replay, crash, stale baseline, rollback and Undo.
- [ ] Verify RED with responsibility/dependency assertions before extraction.
- [ ] Extract pure transitions first, then scheduler, persistence interaction and reconciliation one commit at a time.
- [ ] Run all Rust/C++ transaction suites, approval boundary tests and architecture guard.
- [ ] Compare preview digests, journals, status frames and post-Undo scene bytes; reject any mismatch.

### Task 6: Extract operations, setup, doctor and capture

**Files:**
- Split operations setup/doctor modules and surface-capture lease modules from §4.
- Modify `godot-codex-mcp/crates/godot-codex/src/main.rs` into CLI parsing plus composition only.
- Test: setup/doctor/package/capture lease and fault-matrix suites.

**Interfaces:**
- Pure policy returns immutable plans/reports.
- Filesystem/process/environment/clock ports are owned inward; adapters preserve no-follow, atomic-write, private-permission and bounded-process guarantees.
- Setup retains preview/digest apply/repair semantics; capture retains claim/finalize/cancel/abandon semantics.

- [ ] Add failing tests for I/O-free policy evaluation and explicit adapter injection.
- [ ] Add characterization for setup preview/apply/repair, all five stable doctor faults and capture cleanup.
- [ ] Extract parsing, policy, plan, I/O and rendering in separate commits.
- [ ] Run operations, surface-capture, package and Sprint 11 fault tests plus architecture guard.
- [ ] Verify doctor latency, capture cleanup and byte-identical owned config/receipt output.

### Task 7: Make qualification manifest-bound and version-resilient

**Files:**
- Create `tests/codex/sprint11_release_coordinate.py` and its tests.
- Modify `tests/codex/sprint11_technical_acceptance.py`.
- Modify `tests/codex/sprint11_host_delta_qualify.py`.
- Modify `tests/codex/schemas/sprint11-technical-private-alpha.schema.json`.
- Modify only directly affected qualification tests; do not relabel historical evidence.

**Interfaces:**
- `ReleaseCoordinate.from_manifest(path: Path) -> ReleaseCoordinate` binds package version, source commit, target triple, package manifest digest, Godot version/commit/hash, registry digest and compatibility digest.
- Для текущей release line `0.1.x` значение `artifact_label` вычисляется как `f"v01{patch:02d}"`; `0.1.19` остаётся `v0119`, а `0.1.20` становится `v0120`. Версия вне `0.1.x` fail-closed требует новой naming-policy revision, чтобы не породить collision или молча переименовать historical evidence.
- Validators receive a `ReleaseCoordinate`; no production package version literal remains in current qualification code.

- [ ] Write failing tests for `0.1.20 -> v0120`, invalid SemVer, manifest digest mismatch, evidence/package mismatch and immutability of `v0119` paths.
- [ ] Run the three targeted Python unit suites and verify RED against the fixed `0.1.19` checks.
- [ ] Implement the frozen dataclass/loader and replace fixed version checks with exact manifest binding while preserving the evidence JSON shape and schema version.
- [ ] Run all Sprint 11 qualification unit tests and a synthetic `0.1.20` validation; require historical `v0119` validation to remain unchanged.
- [ ] Commit tooling/tests without generated `v0120` evidence.

### Task 8: Close ratchets and qualify package 0.1.20

**Files:**
- Modify workspace/package version coordinates and current architecture/component documentation.
- Modify Bridge/package manifests only through existing package-owned workflows.
- Create new artifacts only under `tests/codex/acquisition/sprint11/*-v0120/` and `technical-private-alpha-v0120/`.

**Interfaces:**
- Package manifest is the sole release coordinate authority.
- Final report retains `schema_version: s11-technical-private-alpha/1.0` and frozen claim names.

- [ ] Remove every non-declarative LOC ratchet entry; verify all production files are `<= 1200` logical LOC.
- [ ] Run `cargo fmt --check`, `cargo test --workspace --all-targets`, Clippy with `-D warnings`, release builds, architecture checker and all Python unit/acceptance tests.
- [ ] Build Godot with module enabled and disabled, run the full C++ Bridge suite, and run RPC `1.0`-`1.8` conformance.
- [ ] Repeat the Task 1 performance suite and require every absolute and relative gate to pass.
- [ ] Build the frozen macOS arm64 package `0.1.20`, verify reproducibility and bind the exact Godot Bridge binary hash.
- [ ] Acquire fresh package-live, same-project, multi-project, host smoke, compatibility-bundle and reproducibility receipts for `v0120`.
- [ ] Generate `technical-private-alpha-v0120/report.json` and require `status: passed`, `technical_private_alpha: true`, `external_codex_beta: false`, `commercial_ready: false`, `usability: deferred_unacquired`.
- [ ] Confirm the worktree is clean and historical `v0119` artifacts are byte-identical to their pre-Sprint digests.

---

## 6. Per-wave mandatory verification

Каждая Task 2-7 branch проходит:

```bash
cargo fmt --manifest-path godot-codex-mcp/Cargo.toml --all --check
cargo test --manifest-path godot-codex-mcp/Cargo.toml --workspace --all-targets
cargo clippy --manifest-path godot-codex-mcp/Cargo.toml --workspace --all-targets -- -D warnings
python3 -m unittest discover -s tests/codex -p 'test_*.py'
python3 tests/codex/architecture_guard.py --policy tests/codex/architecture/production-boundaries.toml
```

Relevant Godot test binary is rebuilt with:

```bash
.venv/bin/scons platform=macos arch=arm64 target=editor dev_build=yes tests=yes module_codex_bridge_enabled=yes accesskit=no angle=no vulkan=no -j8
bin/godot.macos.editor.dev.arm64 --headless --test '--test-case=*[CodexBridge]*' --no-colors
```

Module-disabled/export guard выполняется отдельно теми же canonical flags из Sprint 11 acceptance; новый hand-written source остаётся в каталогах, которые собирает `modules/codex_bridge/SCsub`.

---

## 7. Final acceptance

Sprint 11.5 считается завершённым только одновременно при выполнении всех условий:

- dependency direction и port ownership из §2 доказаны checker/tests;
- ни одного non-declarative production file больше `1200` logical LOC;
- все historical behavior traces совпадают после нормализации;
- все published SLO и relative performance gates проходят;
- package `0.1.20` воспроизводим, integrity-verified и exact Bridge-bound;
- current qualification tooling не содержит fixed package-version assumption;
- `technical-private-alpha-v0120/report.json` имеет `status: passed`;
- нет смешанных extraction/behavior commits;
- backlog содержит все найденные, но не blocking проблемы;
- Sprint 12 не начат внутри Sprint 11.5.

## Assumptions

- Logical index schema `1.3`, segment format, journals, config и setup receipt shape не мигрируют.
- Existing public MCP registry/output schema и Bridge RPC remain frozen.
- Изменение внутреннего ownership может потребовать один ADR-approved ports crate; архитектурная цель не оправдывает дополнительные crates без compile-time доказательства.
- Если qualification выявляет изменение Codex host coordinate, используется signed/hashed host measurement и manifest-bound compatibility flow, а не ручная замена version constants.
