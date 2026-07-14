# ARCHITECTURE-001 — Bridge, sidecar, семантический индекс и evidence model

**Статус:** Draft for review 0.1

**Дата:** 2026-07-14

**Целевой релиз:** 1.0

**Родительские документы:** [MASTER_SPRINT_ROADMAP.md](MASTER_SPRINT_ROADMAP.md), разделы 5.1–5.5 и 9; [PRODUCT-001-semantic-bridge-vision-and-plan.md](PRODUCT-001-semantic-bridge-vision-and-plan.md); [RELEASE-001-release-1.0-acceptance-and-evidence-plan.md](RELEASE-001-release-1.0-acceptance-and-evidence-plan.md)

**Владельцы решения:** Engine/Editor, Sidecar/Protocol, Index, QA и Security — назначаются до начала Sprint 1

---

## 1. Назначение документа

Этот документ превращает архитектурный контур разделов 5.1–5.5 мастер-плана в техническую спецификацию и исполнимый план. Он фиксирует:

- границы `modules/codex_bridge`, локального Bridge RPC и `godot-codex-mcp`;
- единственный допустимый путь чтения и изменения внутреннего состояния Godot Editor;
- lifecycle editor session, handshake, snapshot, event stream, reconnect и backpressure;
- правила revisions и согласованности между bridge и sidecar;
- логическую модель семантического индекса;
- нормативную форму fact/evidence, confidence и freshness;
- threading, security, performance и recovery invariants;
- этапы реализации, зависимости, issue-ready backlog и доказательства готовности.

Документ является авторитетным для границ компонентов и их сквозного взаимодействия. Он не заменяет:

- `ADR-001` — выбор языка, packaging и topology sidecar;
- `PROTOCOL-001` — окончательный wire encoding, framing и schema Bridge RPC v1;
- `MCP-001` — model-facing tools, resources и approval annotations;
- `INDEX-001` — физическую schema, storage engine и migrations;
- `EVIDENCE-001` — полный source vocabulary и правила агрегации;
- `EDITOR-001`, `RUNTIME-001` и `WRITE-001` — доменные API editor, runtime и transactions.

При расхождении документов действует порядок из `PRODUCT-001`: мастер-план определяет scope и gates, `PRODUCT-001` — продуктовые инварианты, этот документ — component contract, утверждённые дочерние спецификации — wire/API детали, `RELEASE-001` — способ доказательства релизной готовности.

## 2. Цели и non-goals

### 2.1. Цели

1. Изолировать нестабильные internal editor API за узким C++ adapter layer.
2. Не блокировать editor тяжёлой индексацией, графовыми запросами или MCP lifecycle.
3. Передавать sidecar согласованные bounded snapshots и ordered deltas.
4. Исключить смешивание project, editor и runtime sessions.
5. Сделать reconnect и rebuild штатным, проверяемым состоянием.
6. Обеспечить evidence-backed ответы при открытом и закрытом editor.
7. Разрешать write только через revision-guarded editor-native transaction.
8. Сохранить один семантический контракт для App, CLI, IDE и Godot Dock.

### 2.2. Non-goals

- Bridge RPC не является MCP, JSON-RPC API для сторонних клиентов или публичным network API.
- Bridge не запускает модель, не хранит prompts/threads и не знает об OpenAI API или пользовательской авторизации.
- Sidecar не воспроизводит Godot editor semantics raw-парсингом `.tscn`, если доступен авторитетный Godot source.
- Sidecar не меняет открытую сцену прямой записью файла.
- Индекс не является единственным источником истины для dirty editor state или активного runtime.
- В v1 не гарантируется replay произвольной истории событий после потери соединения; full snapshot является обязательным recovery primitive.
- Физический storage engine и язык sidecar не выбираются неявно этим документом.

## 3. Нормативные архитектурные инварианты

### 3.1. Единственный владелец Godot internals

Только editor-only `modules/codex_bridge` может напрямую обращаться к `EditorNode`, `EditorData`, `EditorSelection`, Inspector, `EditorFileSystem`, GDScript internals, `EditorDebugger` и `EditorUndoRedoManager`.

Sidecar, Codex surfaces и embedded Dock не получают указатели, `ObjectID`, private headers или альтернативный write path. Внешний код оперирует только opaque entity IDs, revisions и сериализованными facts.

### 3.2. Один writer, несколько проекций

- Bridge является единственным writer live editor state.
- Sidecar является единственным writer своего project-scoped semantic index.
- Runtime является отдельной projection с обязательным `runtime_session_id`.
- MCP clients читают и инициируют operations, но не владеют ни editor, ни index state.

### 3.3. Project/session boundary

Каждое соединение доказывает соответствие canonical project root и текущей editor session. Нельзя выбирать editor по имени каталога, последнему открытому окну или частичному совпадению пути.

Каждый request после handshake неявно связан с:

- `project_id`;
- `editor_session_id`;
- согласованной protocol version;
- аутентифицированным connection ID;
- заявленными capabilities.

Несоответствие любой координаты является ошибкой, а не поводом выбрать другой editor автоматически.

### 3.4. Protocol separation

Bridge RPC оптимизирован для Godot internals, snapshot/event replication и transaction execution. MCP оптимизирован для model-facing queries, resources, approvals и ограниченного контекста. Методы не обязаны иметь соответствие один к одному.

Новая capability считается общей только после появления:

1. bridge adapter и Bridge RPC contract;
2. нормализованной модели sidecar;
3. MCP representation;
4. contract/parity tests.

### 3.5. Revision before mutation

Read response сообщает revision vector. Write request содержит preconditions на тот же project/editor/scene state. Bridge повторно проверяет их непосредственно перед первой mutation.

### 3.6. Evidence before prose

Sidecar сначала создаёт structured entities, facts и evidence. Summary для модели и текст UI являются производными и не могут быть единственным носителем результата.

### 3.7. Bounded by construction

Каждый request, queue, snapshot chunk, Variant, tree traversal и query имеет размер, глубину, timeout/cancellation policy и structured truncation. Неограниченная сериализация editor tree запрещена.

## 4. Контекст системы и trust boundaries

```text
┌────────────────────────────────────────────────────────────────────┐
│ Codex surfaces                                                     │
│ App │ CLI │ supported IDE │ Godot Dock via Codex app-server       │
└──────────────────────────────┬─────────────────────────────────────┘
                               │ stdio MCP
                               ▼
┌────────────────────────────────────────────────────────────────────┐
│ godot-codex-mcp — project-scoped sidecar                           │
│ MCP facade │ query engine │ semantic index │ evidence │ approvals │
│ Bridge client │ snapshot replicator │ offline/read-only mode       │
└──────────────────────────────┬─────────────────────────────────────┘
                               │ private versioned local Bridge RPC
                               ▼
┌────────────────────────────────────────────────────────────────────┐
│ modules/codex_bridge — editor-only C++ module                      │
│ auth/transport │ main-thread dispatcher │ adapters │ transactions  │
│ snapshots │ revisions/events │ debugger/runtime observation       │
└──────────────────────────────┬─────────────────────────────────────┘
                               │ internal C++ API
                               ▼
┌────────────────────────────────────────────────────────────────────┐
│ Godot Editor                                                       │
│ EditorData/Selection │ Inspector │ ResourceUID │ SceneState        │
│ GDScript │ EditorDebugger │ EditorUndoRedoManager                  │
└────────────────────────────────────────────────────────────────────┘
```

Trust boundaries:

- **B1 — MCP client → sidecar.** Standard MCP request, Codex approval semantics и project-scoped process launch.
- **B2 — sidecar → bridge.** Локальный endpoint, capability token, exact project binding и version negotiation.
- **B3 — bridge worker → Godot main thread.** Только typed command queue; transport code не обращается к editor objects.
- **B4 — static index → live/runtime overlays.** Persisted facts не становятся current editor/runtime facts без подходящей session coordinate.
- **B5 — plan → apply.** Approval не заменяет повторную проверку revisions внутри bridge.

## 5. Godot Codex Bridge

### 5.1. Внутренняя декомпозиция

| Компонент | Ответственность | Допустимый thread |
|---|---|---|
| `CodexBridgeService` | Lifecycle модуля, editor session, capability registry, shutdown | Main thread |
| `BridgeTransportServer` | UDS/Named Pipe/loopback accept, framing, I/O, connection limits | I/O worker |
| `SessionAuthenticator` | Discovery, challenge/proof, token rotation, project binding | I/O worker + main-thread initialization |
| `MainThreadDispatcher` | Bounded очередь typed commands/results | Producer I/O, consumer main thread |
| `RevisionClock` | `event_seq`, project/scene/operation revisions, runtime session lifecycle | Main thread |
| `SnapshotCoordinator` | Consistent cut, chunking, tombstones, resync | Main thread + serialization worker только для detached data |
| `EditorContextAdapter` | Open scenes, current scene, selection, dirty state, active script | Main thread |
| `InspectorAdapter` | Текущий inspected object, property metadata/value и safe Variant projection | Main thread |
| `ResourceGraphAdapter` | `EditorFileSystem`, `ResourceUID`, dependencies, imports | Main thread для editor API; detached enrichment вне main thread |
| `SceneStateAdapter` | `PackedScene`/`SceneState`, nodes, owners, groups, connections, instances | Main thread при live object access |
| `GDScriptSemanticAdapter` | Parser/analyzer/LSP context, symbols, references, diagnostics | Main thread или engine-approved worker path |
| `RuntimeAdapter` | Run/debug lifecycle, remote tree/objects, diagnostics, stack | Main thread |
| `TransactionExecutor` | Preconditions, preview support, apply, Undo/Redo и status reconciliation | Main thread |
| `VariantProjector` | Allowlist, depth/size limits, redaction и opaque handles | Main thread capture, detached encoding после копирования |
| `BridgeEventJournal` | Небольшой in-memory ordered buffer для delivery/ack, не semantic index | Main thread append, I/O worker delivery |

Внутренние adapters не возвращают editor pointers наружу. Они создают immutable DTO с ограниченным набором Godot-independent scalar/container types.

### 5.2. Lifecycle

```text
disabled/non-editor
    │ editor build + project loaded
    ▼
starting → endpoint_ready → accepting → authenticated → synchronizing → ready
    │              │             │             │             │
    └──────────────┴─────────────┴─────────────┴─────────────┴→ stopping → stopped
```

1. Модуль регистрируется только на `MODULE_INITIALIZATION_LEVEL_EDITOR` под `TOOLS_ENABLED`.
2. После загрузки project root bridge создаёт `editor_session_id`, token и private runtime directory.
3. Endpoint начинает слушать до публикации discovery record.
4. Discovery record публикуется атомарным rename только после успешного bind/listen.
5. До handshake разрешены только protocol/auth messages с жёстким timeout.
6. После handshake sidecar обязан запросить initial full snapshot.
7. При смене project root, закрытии editor или shutdown bridge прекращает accept, инвалидирует token, удаляет discovery/socket и завершает worker с bounded timeout.
8. Export templates и non-editor builds не содержат активного bridge service или discovery side effects.

В v1 один canonical project root может иметь только один опубликованный bridge endpoint. Bridge удерживает project-local session lock; второй editor process не перезаписывает discovery/token, а показывает structured diagnostic и остаётся без publishable endpoint. Поддержка нескольких editor instances одного project требует отдельного session registry и явного выбора клиента и не добавляется скрытой эвристикой.

### 5.3. Проверенные точки интеграции текущего форка

| Домен | Основная опора в исходниках | Использование bridge |
|---|---|---|
| Открытые сцены | `editor/editor_data.h` | `get_edited_scene_count`, `get_edited_scene_root`, history ID, dirty state |
| Selection | `EditorNode::get_editor_selection`, `EditorSelection::get_selected_nodes` | Current/multi-selection с session-scoped identity |
| Inspector | `InspectorDock::get_inspector_singleton`, `EditorInspector::get_edited_object` | Inspected object и выбранные editable properties |
| Файловая модель | `editor/file_system/editor_file_system.h` | Scan/update signals, import metadata, filesystem projection |
| UID/dependencies | `core/io/resource_uid.h`, `ResourceLoader::get_dependencies` | Stable resource identity и dependency edges |
| Scene semantics | `scene/resources/packed_scene.h` | Base scene, nodes, owner paths, instances, groups, properties, connections |
| GDScript | `modules/gdscript/gdscript_parser.h`, `gdscript_analyzer.h`, `language_server/gdscript_workspace.h` | Parser/analyzer facts и доступный workspace symbol context |
| Runtime/debugger | `editor/debugger/editor_debugger_node.h`, `script_editor_debugger.h` | Session lifecycle, remote tree/object, stack/errors |
| Transactions | `editor/editor_undo_redo_manager.h` | Action history, do/undo methods/properties, commit/undo/status |
| Unix transport | `core/io/uds_server.h`, `stream_peer_uds.h` | UDS server на macOS/Linux |
| Windows pipe basis | `drivers/windows/file_access_windows_pipe.*` | Опора для отдельного Named Pipe adapter; пригодность подтверждается spike |

Эти API являются internal и могут меняться при upstream sync. Все include/use должны быть сосредоточены в adapters; protocol DTO и sidecar не зависят от конкретного layout Godot headers.

### 5.4. Threading model

Нормативный поток request:

```text
I/O worker: decode → authenticate → validate limits → enqueue typed command
main thread: dequeue within frame budget → inspect/mutate → create detached DTO
worker: encode detached DTO → apply output limits → send
```

Правила:

- I/O worker не разыменовывает `ObjectID` и не вызывает `ObjectDB`, `EditorNode` или editor singleton.
- Main thread обрабатывает ограниченное число commands и ограниченный микросекундный budget за frame.
- `ObjectID` разрешается только на main thread и повторно проверяется перед чтением/записью.
- Большая snapshot traversal разбивается на resumable slices.
- После detach допускаются compression, hashing, framing и socket write вне main thread.
- Cancellation удаляет queued/read work до начала исполнения. После transaction commit point клиент получает status по `transaction_id`; слепая отмена или повтор apply запрещены.
- Shutdown закрывает accept, отменяет read jobs, дожидается или reconcile-ит active write и только затем разрушает adapters.

### 5.5. Safe Variant projection

Bridge сериализует только утверждённую проекцию Variant:

- scalar, vectors/transforms/colors и bounded arrays/dictionaries;
- resource references как entity/UID/path reference, а не полное рекурсивное содержимое;
- object references как typed opaque handle с identity scope;
- strings и byte arrays после length/redaction policy;
- циклы через `reference_id`, без повторного разворачивания;
- unsupported/oversized values как metadata `{type, omitted_reason, size_hint}`.

Параметры запроса могут сужать fields/depth, но не повышать server hard limit.

### 5.6. Наблюдение и запись

Read adapter может наблюдать native actions и менять revisions, но не обещает восстановить неизвестный payload стороннего plugin. Полная operation history доступна только для bridge transactions.

Write lifecycle:

```text
prepare → previewed → awaiting_approval → applying → applied
      → validating → committed → undone
                       └→ validation_failed → rolled_back/needs_attention
```

Непосредственно перед `create_action`/apply `TransactionExecutor` проверяет:

- project/editor session;
- scene identity и scene revision;
- expected object/entity existence и types;
- guarded file hashes для script/resource patches;
- approval receipt, preview digest, scope и expiry;
- idempotency key и отсутствие уже применённого результата.

Scene operations регистрируют do/undo methods/properties и references в `EditorUndoRedoManager`. Если составной change set включает файл, `WRITE-001` обязан определить guarded apply и inverse data; raw file write не получает обещание editor-native atomic Undo.

## 6. godot-codex-mcp sidecar

### 6.1. Ответственность

Sidecar:

- запускается как stdio MCP server для одного canonical project root;
- обнаруживает discovery record только внутри этого project;
- выполняет handshake и capability negotiation;
- применяет snapshot/events в нормализованный индекс;
- планирует semantic queries и агрегирует evidence;
- создаёт token-bounded model summaries из structured facts;
- публикует MCP tools/resources/status;
- хранит prepared transaction и маршрутизирует approval/apply;
- поддерживает честный offline/read-only mode;
- редактирует logs до записи и MCP output;
- не содержит второй реализации live Godot semantics.

### 6.2. Внутренняя декомпозиция

| Компонент | Ответственность |
|---|---|
| `ProjectLocator` | Canonical root, project fingerprint, discovery path |
| `BridgeClient` | Transport, handshake, requests, cancellation, reconnect |
| `CapabilityRegistry` | Пересечение bridge/sidecar/MCP capabilities |
| `SnapshotReplicator` | Staging, chunk validation, atomic activation, gap handling |
| `EventApplier` | Ordered event batches, invalidation и overlay updates |
| `IndexStore` | Physical persistence через storage abstraction |
| `SemanticNormalizer` | Godot DTO → canonical entities/relations/facts |
| `EvidenceAggregator` | Provenance, deduplication, conflicts, confidence/freshness |
| `QueryEngine` | Traversal, filters, pagination, consistency modes |
| `McpFacade` | Tools, resources, structured errors и output budgets |
| `TransactionCoordinator` | Plan/preview/approval/apply/status/validation/undo |
| `Redactor` | Token/path/value/log sanitation |
| `HealthReporter` | Status, progress, queue/latency metrics без project content |

Выбор языка, runtime и repository topology делается в `ADR-001`. Независимо от выбора production artifact должен быть отдельным single-binary process без требования устанавливать language runtime пользователю.

### 6.3. Sidecar state machine

```text
starting → locating → connecting → authenticating → full_sync → ready
              │            │              │            │
              └────────────┴──────────────┴────────────┴→ offline
ready → reconnecting → full_sync → ready
ready/full_sync → incompatible | rebuilding | overloaded | stopping
```

MCP status всегда отражает фактическое состояние. `offline`, `rebuilding`, `incompatible` и `overloaded` не маскируются пустым успешным ответом.

### 6.4. Маршрутизация write approval

Approval принадлежит model-facing contract и пользовательской поверхности, а не OpenAI-aware коду внутри Godot:

1. Sidecar строит transaction plan и получает от bridge immutable preview с revision preconditions и digest.
2. MCP surface показывает semantic preview, risk и affected entities/files.
3. После явного approval sidecar отправляет отдельный apply request с `transaction_id`, preview digest, approval scope и expiry.
4. Bridge не знает Codex account/user identity, но проверяет, что apply относится к подготовленной transaction, digest совпадает, approval scope достаточен, receipt не истёк, а revisions current.
5. После commit point sidecar маршрутизирует status/validation/Undo; потеря response приводит к reconciliation по transaction ID.

Capability token доказывает право локального sidecar обращаться к editor, но не заменяет user approval. Точный approval receipt и trust relationship MCP host ↔ sidecar фиксируются в `MCP-001`/`WRITE-001`; bridge никогда не принимает свободный текст «пользователь согласен».

### 6.5. Offline/read-only mode

Offline mode разрешён, если:

- project binding доказан по текущему root;
- index schema совместима;
- сохранённый static generation завершён атомарно;
- каждый result маркирует отсутствие editor/runtime layers;
- facts из предыдущей live/runtime session имеют `freshness: stale` или исключены;
- все write, run/debug и consistent-live queries отклоняются `editor_offline`.

## 7. Локальный Bridge RPC

### 7.1. Слои протокола

1. **Transport:** UDS, Named Pipe или защищённый loopback fallback.
2. **Framing:** bounded message envelope; окончательное encoding фиксирует `PROTOCOL-001`.
3. **Session:** discovery, challenge/proof, project binding, version/capabilities.
4. **Messaging:** request/response, notification, cancellation и ack.
5. **Replication:** full snapshot, ordered events, revisions и resync.
6. **Operations:** transaction prepare/apply/status/undo.

MCP names, prompts, tool annotations и UI events не входят в Bridge RPC schema.

### 7.2. Discovery и token

Runtime layout внутри Godot project data directory:

```text
.godot/codex/
├── bridge.json
├── run/
│   └── bridge-<editor_session_id>.sock
├── session.token
└── index/                    # либо index.sqlite после INDEX-001 spike
```

Минимальный discovery record:

```json
{
  "discovery_schema": 1,
  "project_id": "project:sha256:<fingerprint>",
  "editor_session_id": "editor:01J...",
  "pid": 12345,
  "transport": "uds",
  "endpoint": ".godot/codex/run/bridge-editor-01J.sock",
  "protocol_versions": ["1.0"],
  "token_file": ".godot/codex/session.token",
  "created_at": "2026-07-14T10:00:00Z"
}
```

Security rules:

- directory/file permissions — owner-only (`0700`/`0600`) на Unix и current-user DACL на Windows;
- token — минимум 256 random bits, новый для каждой editor session;
- token хранится отдельно от discovery metadata и никогда не попадает в argv, MCP, model context, crash text или обычные logs;
- handshake передаёт proof/challenge, а не печатаемое значение token; точный алгоритм фиксирует `PROTOCOL-001` и security review;
- discovery и token публикуются атомарно, инвалидируются при shutdown и не считаются доверенными без проверки PID/session/handshake;
- setup/doctor проверяет, что `.godot/` не попадает в Git, но не полагается на Git ignore как на access control.
- project-local lock не позволяет второму editor process заменить active discovery record; stale lock проверяется по PID и handshake, а не удаляется только по возрасту файла.

### 7.3. Transport policy

| Платформа | Основной transport | Fallback | Обязательная защита |
|---|---|---|---|
| macOS | Unix Domain Socket | loopback TCP только при документированной несовместимости | owner-only directory/socket + token proof |
| Linux | Unix Domain Socket | loopback TCP | owner-only directory/socket + token proof |
| Windows | Named Pipe | loopback TCP | current-user DACL или random loopback port + token proof |

Loopback fallback:

- bind только `127.0.0.1`/`::1`;
- random OS-assigned port;
- endpoint публикуется только в private discovery;
- не включается автоматически при permission/configuration error, который может ослабить boundary;
- platform acceptance проверяет отсутствие non-loopback listener.

Текущий форк содержит `UDSServer`/`StreamPeerUDS`; пригодность существующего Windows pipe abstraction для bidirectional server lifecycle является отдельным spike. Если adapter не обеспечивает корректные ACL, cancellation и reconnect, реализуется узкий platform-specific transport внутри модуля либо применяется защищённый loopback fallback.

### 7.4. Handshake

```text
sidecar                              bridge
   │ client.hello                      │
   │ versions, project_id, nonce ─────►│
   │                                   │ verify discovery/session/project
   │◄────────────── server.challenge   │
   │ challenge, server nonce           │
   │ client.authenticate(proof) ──────►│
   │                                   │ constant-time verify
   │◄──────────────── server.ready     │
   │ selected version, capabilities,   │
   │ editor session, revision vector   │
   │ snapshot.request(full) ──────────►│
```

До `server.ready` запрещены semantic/read/write methods. Authentication failure закрывает connection без раскрытия, какая часть proof была неверной. Version mismatch возвращает только совместимые ranges и remediation metadata без project state.

### 7.5. Logical message envelope

Окончательный wire format определяет `PROTOCOL-001`, но логическая schema v1 обязана различать:

```json
{
  "schema_version": "1.0",
  "kind": "request",
  "request_id": "req:01J...",
  "method": "editor.snapshot.get",
  "deadline_ms": 5000,
  "params": {},
  "context": {
    "project_id": "project:sha256:<fingerprint>",
    "editor_session_id": "editor:01J..."
  }
}
```

Message kinds:

- `request` — уникальный ID, method, params, deadline;
- `response` — тот же ID, result или structured error;
- `notification` — event/session/sync lifecycle без response;
- `cancel` — request ID и reason;
- `ack` — обработанный event batch/chunk window;
- `chunk` — часть snapshot или bounded bulk response.

Request IDs уникальны в connection. Transaction IDs и idempotency keys живут дольше connection и не заменяются request ID.

### 7.6. Capabilities

Минимальные capability families:

- `editor.context`;
- `editor.inspector`;
- `resource.uid_dependencies`;
- `scene.packed_state`;
- `script.gdscript_parser`;
- `script.gdscript_analyzer`;
- `script.language_server_context`;
- `runtime.debugger`;
- `runtime.viewport_capture`;
- `transaction.scene_v1`;
- `sync.full_snapshot_v1`;
- `sync.event_stream_v1`;
- platform transport capability.

Capability содержит version и optional limits. Наличие capability не означает readiness: response/status отдельно сообщает `ready`, `indexing`, `runtime_inactive`, `read_only` и другие operational states.

### 7.7. Revisions и sequences

| Координата | Владелец | Когда меняется | Scope |
|---|---|---|---|
| `event_seq` | Bridge | На каждое опубликованное событие | `editor_session_id` |
| `project_revision` | Bridge | Принятая индексируемая disk/editor mutation | `editor_session_id` в v1 |
| `scene_revision` | Bridge | Structural/property edit конкретной live scene, включая Undo/Redo | editor session + scene identity |
| `runtime_session_id` | Bridge | Новый run/debug process | Один runtime lifecycle |
| `runtime_event_seq` | Bridge | Runtime event/observation update | `runtime_session_id` |
| `operation_seq` | Bridge | Native action/bridge transaction lifecycle change | `editor_session_id` |
| `index_revision` | Sidecar | Успешный atomic index commit | project + index generation |

Правила:

- `event_seq` начинается с 1 и не сбрасывается при reconnect к тому же editor process.
- Revision сравнивается только вместе с её session scope. Новый editor process может начать counters заново и всегда получает новый `editor_session_id`.
- Sidecar не изобретает `scene_revision`; он хранит значение bridge и собственный `index_revision`.
- Один event может увеличить несколько domain revisions, но получает ровно один `event_seq`.
- Coalescing не меняет итоговую revision и сообщает покрытый диапазон sequence.
- Event gap, rollback невозможного batch или несовпадение session немедленно инвалидирует current overlay.

Session-scoped counters являются безопасным baseline v1. `D-03` может выбрать persistent project revision epoch, но клиент всё равно обязан сравнивать revision вместе с `project_id` и session/epoch coordinate; одно числовое значение никогда не доказывает freshness между editor processes.

### 7.8. Full snapshot и resync

В Bridge RPC v1 reconnect всегда завершается full snapshot. `PRODUCT-001` допускает delta replay как оптимизацию совместимого cache; она добавляется только отдельной capability после доказательства непрерывности и не отменяет full snapshot как обязательный recovery primitive.

Snapshot flow:

1. Sidecar отправляет `snapshot.request` с domains и limits.
2. Bridge фиксирует `snapshot_id`, `base_event_seq` и revision vector.
3. Bridge time-sliced собирает immutable chunks; новые events получают sequence выше base и временно буферизуются.
4. `snapshot.begin` сообщает counts/limits; `snapshot.chunk` передаёт entities/facts/tombstones; `snapshot.end` — checksum и final metadata.
5. Sidecar валидирует session, order, chunk checksums и counts в staging generation.
6. Sidecar атомарно активирует generation.
7. Sidecar применяет buffered events строго после `base_event_seq` и отправляет ack.

Неполный, отменённый или повреждённый snapshot никогда не становится current. Если bridge не может удержать post-snapshot events в bounded journal, он отправляет `sync.invalidated`; sidecar повторяет snapshot.

### 7.9. Backpressure и начальные limits

Начальные значения являются implementation defaults и калибруются в `PERFORMANCE-001`; повышение не меняет hard safety invariant.

| Limit | Начальное значение |
|---|---:|
| Обычный message envelope | 1 MiB |
| Negotiated hard maximum одного message | 8 MiB |
| Snapshot chunk payload | 512 KiB |
| Одновременные requests одного connection | 64 |
| Control queue | 64 messages |
| Event journal | 4096 events или 16 MiB |
| Bulk/snapshot outbound window | 32 MiB |
| Variant depth | 8 |
| Container items до pagination/truncation | 1000 |

При high-water mark:

1. control/status/cancel сохраняют высший приоритет;
2. idempotent property updates могут coalesce по `(entity, property)`;
3. structural, revision, transaction и session events не отбрасываются;
4. bulk producer ждёт credits/ack или отменяется по deadline;
5. невозможность сохранить непрерывность приводит к `sync.invalidated`, а не к тихой потере.

### 7.10. Structured errors

Минимальный каталог:

| Код | Смысл | Retry |
|---|---|---|
| `unauthenticated` | Handshake/token proof не завершён | Только новый handshake |
| `project_not_bound` | Project ID/root не совпал | Нет, нужен setup/doctor |
| `session_mismatch` | Editor/runtime session устарела | Full reconnect/snapshot |
| `protocol_mismatch` | Нет совместимой версии | После upgrade/downgrade |
| `capability_unavailable` | Capability отсутствует | После capability/state change |
| `invalid_request` | Schema/params нарушены | После исправления request |
| `message_too_large` | Hard limit превышен | Сузить/chunk/paginate |
| `deadline_exceeded` | Work не завершён вовремя | Bounded retry для read |
| `cancelled` | Read/queued work отменён | По решению клиента |
| `overloaded` | Queue/window исчерпан | Backoff с jitter |
| `stale_revision` | Preconditions не current | Новый snapshot/preview |
| `snapshot_required` | Overlay не доказан current | Full snapshot |
| `runtime_not_active` | Нет активной runtime session | Запустить runtime |
| `approval_required` | Transaction не одобрена | Approval flow |
| `transaction_in_doubt` | Связь потеряна после commit point | Status reconciliation, без retry apply |
| `result_truncated` | Применён output limit | Cursor/сужение query |

## 8. Семантический индекс

### 8.1. Роль и границы

Индекс является перестраиваемой project-scoped проекцией, а не альтернативным source of truth. Он хранит:

- persistent static facts из Godot resources/scenes/scripts;
- live editor overlay текущей editor session;
- runtime projection текущей runtime session;
- diagnostics, revisions и evidence;
- ingestion checkpoint и schema metadata.

Static generation переживает закрытие editor. Editor/runtime overlays либо удаляются при смене session, либо сохраняются только как stale evidence согласно retention policy.

### 8.2. Логическая storage model

Физическая schema обязана представить минимум:

| Набор | Ключевые поля |
|---|---|
| `index_metadata` | schema version, project ID, generation, build status, source hashes |
| `entities` | entity ID, kind, identity scope, origin, normalized payload, validity |
| `relations` | fact ID, subject, predicate, object/literal, domain, validity |
| `evidence` | evidence ID, fact ID, source/location, confidence, freshness, revisions |
| `resource_identity` | UID, current path, type, import state, content generation |
| `source_documents` | resource/script identity, content hash, parse state |
| `diagnostics` | source/code/location/severity/revision |
| `editor_overlay` | editor session, scene revision, overrides/tombstones |
| `runtime_projection` | runtime session, remote identity, source mapping |
| `ingest_checkpoint` | editor session, last event seq, bridge revision vector, index revision |
| `transactions` | transaction ID, state, affected entities, retained hashes/metadata |

Entity/relation payload не должен дублировать абсолютные paths или секретные values, если для query достаточно normalized reference/hash.

### 8.3. Entity domains

Обязательные entities:

- Project;
- Resource и ResourceUID;
- Scene и SceneInheritance;
- Node definition и session-scoped editor node;
- Script и Symbol;
- Signal и SignalConnection;
- Group;
- ProjectSetting и Autoload;
- InputAction;
- RuntimeObject;
- Diagnostic;
- Transaction;
- Evidence.

Обязательные relation vocabulary наследуется из `PRODUCT-001`: `contains`, `references`, `instantiates`, `inherits`, `overrides`, `attaches_script`, `declares_symbol`, `references_symbol`, `connects_signal`, `belongs_to_group`, `preloads`, `loads`, `calls`, `runtime_instance_of`, `selected_in_editor`, `changed_by_transaction`, `validated_by`, `supersedes_revision`.

### 8.4. Ingestion и atomicity

- Full snapshot строит staging generation и активируется одной storage transaction/atomic pointer swap.
- Event batch применяется только при exact expected previous `event_seq`.
- Event и все производные entity/relation/evidence changes входят в один index commit.
- `index_revision` увеличивается только после durable commit.
- Query не видит half-applied generation.
- Crash между staging и activation оставляет прежнюю current generation.
- Corrupt/incompatible generation изолируется; rebuild не удаляет последнюю заведомо valid generation до успешной замены.

### 8.5. Overlay rules

- Live overlay заменяет только явно покрытые fields/edges.
- Tombstone dirty scene скрывает соответствующий static entity в current view.
- Unsaved entity получает `editor_session` identity scope.
- Runtime property является отдельным fact, а не новым default editor value.
- Conflict disk/live возвращает обе версии и diagnostic.
- После session mismatch overlay не участвует в `current` query.

### 8.6. Invalidation

| Изменение | Минимальная invalidation |
|---|---|
| Resource path/UID/import | Resource entity, dependency edges, owning scenes/scripts |
| Scene save/reimport | Scene generation, node/connection/group edges, dependent instances |
| Dirty scene edit | Только соответствующий overlay subtree/field + affected queries |
| Script content/parse | Script symbols/references/diagnostics и attached-node derived edges |
| ProjectSettings/Autoload/InputMap | Соответствующие project entities и dependents |
| Runtime restart | Вся previous runtime projection становится stale/retired |
| Event gap | Весь current editor overlay до full snapshot |
| Schema migration | Новая generation или explicit compatible migration |

### 8.7. Storage performance spike

`INDEX-001` сравнивает как минимум SQLite (`.godot/codex/index.sqlite`) и directory/segment store (`.godot/codex/index/`) на одинаковых fixtures.

Decision criteria:

- atomic activation и crash recovery;
- incremental write amplification;
- reverse-edge и evidence query latency;
- concurrent MCP reads во время ingestion;
- migration/backup/rebuild complexity;
- artifact size и single-binary packaging;
- macOS/Windows/Linux locking behavior;
- cancellation и corrupt-cache isolation.

До решения query/storage interfaces не должны раскрывать SQL или layout наружу.

## 9. Evidence model

### 9.1. Fact и Evidence

Канонический semantic result разделяет утверждение и основания:

```json
{
  "fact_id": "fact:01J...",
  "subject": "godot:resource:uid://example",
  "predicate": "referenced_by",
  "object": "godot:node-definition:uid://player#<opaque-node-key>",
  "evidence": [
    {
      "evidence_id": "evidence:01J...",
      "path": "res://player/player.tscn",
      "node_path": "Player",
      "property": "stats",
      "line": null,
      "range": null,
      "confidence": "exact",
      "freshness": "current",
      "source": "packed_scene_state",
      "project_revision": 1042,
      "scene_revision": 83,
      "editor_session_id": "editor:01J...",
      "runtime_session_id": null
    }
  ]
}
```

`fact_id` стабилен только в пределах semantic generation, если `EVIDENCE-001` не докажет более сильную идентичность. Clients используют entity/evidence IDs как opaque values.

### 9.2. Confidence

| Значение | Когда допустимо | Пример |
|---|---|---|
| `exact` | Структурный Godot source или однозначный analyzer result | `SceneState` property, UID dependency, resolved symbol |
| `probable` | Статический анализ имеет несколько допустимых targets | Inferred untyped call target |
| `dynamic` | Target/value вычисляется runtime и не разрешён сейчас | `load(variable)`, `get_node(path_expr)` |
| `runtime-confirmed` | Конкретный active runtime session наблюдал связь | Remote object → source instance |

`runtime-confirmed` не повышает автоматически static fact до `exact` и всегда содержит `runtime_session_id`.

### 9.3. Freshness

| Значение | Смысл |
|---|---|
| `current` | Источник подтверждён на заявленной revision/session |
| `stale` | Fact был корректен на предыдущей revision/session, но current не подтверждён |

Confidence и freshness независимы. Допустимо `exact + stale`; недопустимо представлять его как current. Расширение vocabulary (`unknown`, `retired`, `conflicted`) возможно только через versioned schema change; до этого состояние выражается diagnostic/status, а не новым неоговорённым значением.

### 9.4. Source taxonomy и authority

| Source | Авторитетен для | Не доказывает автоматически |
|---|---|---|
| `resource_uid_registry` | Resource identity/path mapping | Текущее live property value |
| `resource_loader_dependencies` | Объявленные resource dependencies | Динамический load target |
| `packed_scene_state` | Serialized scene structure | Dirty unsaved overlay |
| `live_editor_tree` | Current open scene structure | Сохранённое disk state |
| `live_editor_property` | Current inspected/live value | Runtime value |
| `gdscript_parser` | Syntax/declarations/locations | Все semantic references |
| `gdscript_analyzer` | Однозначно разрешённые symbols/types | Dynamic dispatch target |
| `language_server` | Доступный workspace symbol/reference context | Большее, чем гарантирует adapter capability |
| `editor_file_system` | Editor-known file/import state | Runtime use |
| `editor_debugger` | Active runtime observation | Следующую runtime session |
| `undo_redo_manager` | Native action/version state | Неизвестный payload стороннего action |
| `transaction_journal` | Bridge transaction lifecycle | Независимую корректность результата без validation |
| `raw_file` | Байты/line location на content hash | Godot semantic interpretation |

### 9.5. Aggregation, deduplication и conflicts

- Два evidence одного fact сохраняются как два provenance records.
- Dedup key учитывает subject, predicate, object/literal, source location и source revision.
- Одинаковый текст с разными entity IDs не объединяется.
- Более сильный source не удаляет более слабый; query может выбрать effective confidence, сохраняя основания.
- Противоречащие current facts возвращаются вместе с conflict diagnostic и revision coordinates.
- Sidecar не повышает confidence только из-за совпадения нескольких производных источников одного происхождения.
- Inference модели не записывается как Godot fact. Если inference сохраняется для UX, он имеет отдельный kind и ссылки на source facts.

## 10. Mapping Bridge → index → MCP

| Bridge domain | Нормализованная проекция | MCP family |
|---|---|---|
| Status/capabilities | project/session/status/revision | overview/status resources/tools |
| Editor scenes/selection/Inspector | editor entities, overlays, selected facts | editor/current-scene/inspect tools |
| Resources/UID/dependencies | resource entities и reverse edges | dependencies/find-usages |
| SceneState | scenes/nodes/properties/connections/groups | scene graph/inspect/find-usages |
| GDScript/LSP | symbols/references/diagnostics | symbols/find-usages/diagnostics |
| Debugger/runtime | runtime projection и source mapping | run/tree/object/stack/capture |
| Undo/Redo/transactions | operation/transaction facts | preview/apply/status/undo |

MCP response обязан сообщить `capabilities_used`, revision vector, status/partial reasons, entities/facts/evidence, diagnostics и applied limits. Bridge-native DTO не возвращается model-facing без normalization/redaction.

## 11. Security и privacy

### 11.1. Assets

- project files и unsaved editor state;
- runtime values, logs, screenshots и stack data;
- session token и discovery endpoint;
- transaction approval и inverse data;
- semantic index и evidence;
- user identity/Codex auth, которыми bridge не владеет.

### 11.2. Обязательные controls

- local-only transport и owner-only endpoint;
- challenge/proof и exact project/session binding;
- token rotation на каждый editor session;
- no token/path/value in ordinary logs;
- redaction до persistence и до MCP output;
- bounded Variant/runtime/screenshot access;
- read/write capability separation;
- approval и revision checks внутри transaction boundary;
- no auto-retry destructive request after ambiguous disconnect;
- negative tests с двумя projects и случайным локальным process;
- index/discovery cleanup и documented retention;
- отсутствие OpenAI SDK, credentials и auth code в `modules/codex_bridge`.

Threat model подробно фиксируется в `SECURITY-001`, но эти controls являются blocking requirements Sprint 1, а не будущим hardening.

## 12. Performance и resource budgets

Архитектура наследует release SLO из мастер-плана:

- cached semantic query p95 ≤ 300 ms;
- current scene/selection p95 ≤ 500 ms;
- incremental visibility p95 ≤ 2 s;
- status/ping во время indexing p95 ≤ 200 ms;
- 0 необработанных editor stalls сверх согласованного frame budget.

Component budgets:

- main-thread dispatcher измеряет queue wait и execution time каждого adapter command;
- snapshot traversal time-sliced и cancellable;
- index writes и graph queries выполняются вне editor process;
- control lane не блокируется bulk snapshot/indexing;
- query обязательно поддерживает pagination/cursor;
- screenshot/runtime inspection имеет отдельные rate/size limits;
- backpressure metric не содержит project payload.

Численный frame budget и benchmark hardware блокируют Beta, если не зафиксированы в `PERFORMANCE-001`.

## 13. Observability и log sanitation

Разрешённые поля обычного log:

- component/version;
- hashed project ID;
- session ID в сокращённом/opaque виде;
- request method, status/error code, duration, sizes и queue depth;
- revision/sequence numbers;
- transaction ID без values;
- capability names и state transitions.

Запрещены без explicit secure debug opt-in:

- session token/proof;
- абсолютный project path;
- source text, prompts и property/runtime values;
- environment variables;
- full stack locals;
- screenshot pixels;
- authorization headers или Codex account data.

Secure debug bundle имеет отдельную команду, preview содержимого, retention и redaction. Она не включается автоматически при crash.

## 14. Тестовая стратегия

### 14.1. Test layers

- unit: revision clocks, envelopes, limits, redaction, normalization, evidence merge;
- schema/contract: Bridge RPC fixtures и N/N-1 policy;
- module integration: real editor APIs на fixtures;
- bridge ↔ sidecar integration без модели;
- golden index: entities/edges/evidence и migrations;
- process/fault: crash, cancellation, queue saturation, event gap, corrupt snapshot/index;
- transaction fault injection: до/после commit point и reconnect reconciliation;
- security: wrong token, stale discovery, cross-project access, permissions, listener audit;
- performance: main-thread budget, snapshot/index/query latency;
- MCP parity/E2E согласно `RELEASE-001`.

### 14.2. Минимальная component acceptance matrix

| ID | Сценарий | Основное доказательство | Связь с release |
|---|---|---|---|
| `A1` | Wrong token/project/version | Handshake contract + negative process test | PV-10, R1-09 |
| `A2` | Initial full snapshot | Canonical snapshot/index diff | R1-01–R1-03 |
| `A3` | Unsaved Inspector property | Bridge trace + overlay fact/evidence | PV-03, R1-01 |
| `A4` | Resource rename с UID | Pre/post graph + exact evidence | PV-01, R1-02 |
| `A5` | Scene inheritance/instance/override | Golden SceneState graph | PV-02, R1-03 |
| `A6` | GDScript exact/dynamic references | Analyzer truth set, zero false exact | PV-04, R1-02 |
| `A7` | Event gap и reconnect | `sync.invalidated` + full snapshot + no stale current | PV-11, R1-09 |
| `A8` | Queue saturation/cancellation | Bounded memory, control responsiveness | R1-09, performance gate |
| `A9` | Runtime restart | New runtime session + retired old projection | PV-06, R1-04 |
| `A10` | Transaction stale/apply/Undo | Journal + native history + graph diff | PV-08, R1-06/07 |
| `A11` | Editor closed | Honest offline read-only response | PV-12, R1-09 |
| `A12` | Corrupt index | Isolation + deterministic rebuild | PV-11, R1-09 |
| `A13` | Three-OS transport | Permission/listener/reconnect reports | R1-10 |

Unit test не закрывает ни один `R1-*` без требуемого integration/E2E evidence.

## 15. Предлагаемый layout реализации

Godot fork:

```text
modules/codex_bridge/
├── SCsub
├── config.py
├── register_types.cpp
├── register_types.h
├── editor/
│   ├── codex_bridge_service.*
│   ├── main_thread_dispatcher.*
│   ├── revision_clock.*
│   └── snapshot_coordinator.*
├── adapters/
│   ├── editor_context_adapter.*
│   ├── inspector_adapter.*
│   ├── resource_graph_adapter.*
│   ├── scene_state_adapter.*
│   ├── gdscript_semantic_adapter.*
│   ├── runtime_adapter.*
│   └── transaction_executor.*
├── protocol/
│   ├── bridge_messages.*
│   ├── variant_projector.*
│   └── schema_version.*
├── transport/
│   ├── bridge_transport_server.*
│   ├── uds_transport.*
│   └── windows_transport.*
└── tests/
```

Общие schemas и conformance fixtures размещаются так, чтобы их могли собирать C++ bridge и sidecar без копирования вручную. Точное место sidecar и generated code определяется `ADR-001`; рекомендуемая логическая структура:

```text
godot-codex-mcp/
├── bridge-client/
├── index/
├── semantic-model/
├── mcp-server/
├── transactions/
├── schemas/
└── tests/fixtures/
```

Build обязан доказывать отсутствие active module code в export/non-editor target.

## 16. План реализации

План не меняет Sprint 0–17, а детализирует foundation work и его зависимости.

### 16.1. Этапы поставки

| Этап | Спринты | Результат | Exit criteria |
|---|---:|---|---|
| `F0` Decisions/fixtures | S0–S1 | Утверждены ADR-001, logical schema, threat assumptions и fixtures | Нет открытых решений, блокирующих skeleton |
| `F1` Secure bridge skeleton | S1 | Editor-only module, local transport, discovery/auth, request lifecycle | Wrong token/version/project отклоняются; clean shutdown |
| `F2` First vertical slice | S2 | Current scene/selection/unsaved property идут bridge → sidecar → MCP | `A2`, `A3` и `R1-01` trace проходят |
| `F3` Persistent static index | S3 | UID/dependency ingestion, storage decision, rebuild/migration | `A4`, corrupt-cache recovery, cancellation проходят |
| `F4` Semantic core | S4–S6 | Scene/GDScript graph, facts/evidence, find usages | `A5`, `A6`, zero false exact |
| `F5` Live overlay | S7 | Open scenes, Inspector, native history, revisions/events/resync | `A7`, dirty/disk conflict и stale rejection |
| `F6` Runtime projection | S8 | Runtime session/tree/object/stack/diagnostics | `A9`, R1-04/05 Alpha evidence |
| `F7` Safe writes | S9–S10 | Transaction coordinator, editor-native apply/Undo/validation | `A10`, R1-06/07 fault matrix |
| `F8` Production hardening | S14–S15 | Limits, benchmarks, security audit, three transports | `A8`, `A12`, `A13`, Beta gates |

### 16.2. Критический путь

```text
ADR/schema/fixtures
  → editor-only skeleton
  → discovery/auth/transport
  → full snapshot + revision/event model
  → sidecar replication + first MCP slice
  → storage spike + resource graph
  → scene graph → script graph → evidence/query
  → live overlay/resync ─┬→ runtime projection ─┐
                         └→ transaction core ───┴→ validation
  → hardening → Windows/Linux transport → release evidence
```

Нельзя начинать:

- индекс до стабильного snapshot/entity envelope;
- live overlay до revision/gap semantics;
- write до exact project binding и consistent snapshot;
- runtime mapping без session-scoped identity;
- MCP capability без normalized model и contract test;
- storage optimization до correctness/recovery oracle.

### 16.3. Issue-ready backlog

| ID | Приоритет | Задача | Зависимости | Проверяемый результат |
|---|---:|---|---|---|
| `FND-001` | P0 | Утвердить `ADR-001`: sidecar language, packaging, repo topology | Нет | Signed ADR и rejected alternatives |
| `FND-002` | P0 | Создать `PROTOCOL-001` logical/wire schema и compatibility policy | `FND-001` частично | Generated/validated schema + examples |
| `FND-003` | P0 | Создать fixture taxonomy и truth sets для `A1`–`A7` | Нет | Reviewed fixtures independent от implementation |
| `FND-004` | P0 | Зафиксировать project fingerprint и session-scoped revision rules | `FND-002` | Contract tests rename/restart/two projects |
| `BRG-001` | P0 | Создать editor-only module skeleton и build guards | Baseline build | Editor build loads; export/non-editor excludes bridge |
| `BRG-002` | P0 | Реализовать service lifecycle и bounded main-thread dispatcher | `BRG-001` | Shutdown/cancellation/frame-budget unit + integration tests |
| `RPC-001` | P0 | Реализовать macOS UDS transport adapter | `BRG-001` | Bind/accept/reconnect/cleanup test |
| `RPC-002` | P0 | Реализовать private discovery, token rotation и handshake proof | `RPC-001`, `FND-002` | `A1`, permissions и log-redaction tests |
| `RPC-003` | P0 | Реализовать request/response/notification/cancel/deadline | `RPC-002` | Conformance client проходит lifecycle suite |
| `RPC-004` | P0 | Реализовать revision clock, event journal, ack и gap detection | `RPC-003`, `FND-004` | Ordered trace, overflow → `sync.invalidated` |
| `RPC-005` | P0 | Реализовать chunked full snapshot со staging semantics | `RPC-004`, adapters minimum | Cancel/corrupt/reconnect никогда не активирует partial snapshot |
| `ADP-001` | P0 | Editor context + Inspector adapters и safe Variant projection | `BRG-002` | Current scene, selection, dirty/unsaved property evidence |
| `SIDE-001` | P0 | Создать sidecar stdio MCP/BridgeClient skeleton | `FND-001/002` | Initialize/status/shutdown без модели |
| `SIDE-002` | P0 | Реализовать ProjectLocator, capability registry и state machine | `SIDE-001`, `RPC-002` | Exact project binding и honest offline/incompatible status |
| `SIDE-003` | P0 | Реализовать SnapshotReplicator с atomic generation | `SIDE-002`, `RPC-005` | `A2`, reconnect и corrupt chunk tests |
| `MCP-001A` | P0 | Первый MCP slice: editor status/current scene/selection | `ADP-001`, `SIDE-003` | `R1-01` deterministic trace |
| `IDX-001A` | P1 | Провести SQLite vs segment-store spike | `SIDE-003`, benchmark fixture | ADR/INDEX decision с raw metrics и recovery report |
| `ADP-002` | P1 | ResourceUID/EditorFileSystem/dependency adapter | `ADP-001` | `A4`, reverse-edge golden diff |
| `IDX-002` | P1 | Реализовать resource ingestion/invalidation/migration | `IDX-001A`, `ADP-002` | Incremental rename, rebuild cancellation, crash recovery |
| `ADP-003` | P1 | SceneState adapter: inheritance/instances/owners/groups/signals | `IDX-002` | `A5` и order-independent `.tscn` truth |
| `ADP-004` | P1 | GDScript parser/analyzer/LSP adapter | Entity envelope, `ADP-002` | `A6`, parse-error resilience |
| `EVD-001A` | P1 | Реализовать fact/evidence schema и source taxonomy | `ADP-002/003/004` | Every usage has evidence; confidence/freshness independent |
| `QRY-001` | P1 | Query engine, pagination, find usages, conflicts | `EVD-001A` | R1-02/03 benchmark and zero false exact |
| `LIVE-001` | P1 | Live overlay, tombstones, all scene tabs, native history events | `RPC-004`, `QRY-001` | Dirty wins disk; native Undo/Redo revisions correct |
| `LIVE-002` | P1 | Full reconnect/resync and prepared-transaction invalidation | `LIVE-001`, `RPC-005` | `A7`, no stale current after gap |
| `RUN-001` | P1 | Debugger/runtime adapter и session projection | `LIVE-001` | `A9`, stack/source evidence, bounded object inspection |
| `TXN-001` | P1 | Transaction schema/coordinator и approval binding | `LIVE-001`, `EVD-001A` | Preview is read-only; stale apply rejected |
| `TXN-002` | P1 | EditorUndoRedoManager executor + idempotency/status | `TXN-001` | Atomic basic operations and full Undo |
| `TXN-003` | P1 | Validation, fault injection и `in_doubt` reconciliation | `TXN-002`, `RUN-001` | `A10`, R1-07 fault matrix |
| `SEC-001A` | P0 | Threat model transport/discovery/index/transactions | Starts with `FND-002` | No unresolved high finding before Alpha |
| `PERF-001A` | P1 | Instrument dispatcher, queues, snapshot/index/query latency | `BRG-002`, `SIDE-003` | Raw p50/p95 and numeric frame budget |
| `PLAT-001A` | P1 | Windows Named Pipe spike и secure fallback decision | `RPC-003` | ACL/reconnect/cancel/listener report |
| `PLAT-002` | P1 | Linux UDS и Windows production transports | `PLAT-001A`, hardening | `A13` on clean OS runners |
| `QA-001A` | P0 | Protocol recorder, canonical normalizer, fault injector | `FND-002/003` | Machine-readable evidence for every component gate |

P0 foundation tasks до `MCP-001A` образуют минимальный M0 vertical slice. P1 задачи не должны расширять scope P0 до доказанного `R1-01`.

### 16.4. Definition of Ready для implementation task

Task готова к разработке, если:

- указан component owner и затрагиваемая trust boundary;
- вход/выход описан versioned DTO/schema;
- определены thread и memory/size limits;
- перечислены revision/session preconditions;
- есть positive, negative и cancellation/fault case;
- указан fixture и machine-readable evidence;
- breaking change содержит migration/compatibility note.

### 16.5. Definition of Done

Task завершена, если:

- код и generated schemas согласованы;
- internal editor API не утёк за adapter boundary;
- unit/contract/integration tests по риску проходят;
- errors structured, limits/cancellation проверены;
- logs не содержат token/path/project values;
- main-thread time измерен;
- docs обновлены по фактическому поведению;
- acceptance evidence привязано к commit, fixture и component versions;
- known limitation имеет owner и не ослабляет родительский invariant.

## 17. Риски и controls

| Риск | Проявление | Control |
|---|---|---|
| Internal API drift | Upstream sync ломает adapters | Узкий adapter layer + compile/integration fixtures |
| Main-thread stall | Snapshot/Variant traversal замораживает editor | Time slicing, detached DTO, hard limits, telemetry |
| Event loss | Sidecar выдаёт stale overlay как current | Sequence gap → invalidation → full snapshot |
| Cross-project discovery | Читается чужой editor | Project-local discovery + fingerprint + token proof |
| Token leak | Локальный process получает state | Separate token file, redaction, no argv/logs, permissions |
| Session identity reuse | Runtime/editor object указывает на новый process | New session IDs, scoped revisions, full resync |
| Storage corruption | Index недоступен или врёт | Staging/atomic activation, checksums, rebuild oracle |
| Confidence inflation | Dynamic fact становится exact | Source authority table + golden zero-false-exact gate |
| Queue overload | Memory growth или UI starvation | Priority lanes, windows, coalescing, overload/resync |
| Partial transaction | Scene/script расходятся | Preconditions, editor-native Undo, guarded hashes, fault injection |
| Blind retry | Duplicate destructive apply | Idempotency key + `in_doubt` status reconciliation |
| Two semantic implementations | Dock/App расходятся | Один sidecar/index/MCP contract + parity comparator |

## 18. Открытые решения и дедлайны

| ID | Решение | Документ | Срок | Блокирует |
|---|---|---|---:|---|
| `D-01` | Язык, dependency policy, single-binary packaging и repo topology sidecar | ADR-001 | До `SIDE-001` | S1 skeleton |
| `D-02` | Wire encoding/framing и schema generation | PROTOCOL-001 | До `RPC-003` | Conformance tests |
| `D-03` | Project fingerprint и cross-session revision persistence | PROTOCOL-001/INDEX-001 | До S2 acceptance | Cache/reconnect |
| `D-04` | HMAC/proof algorithm и token storage details | PROTOCOL-001/SECURITY-001 | До `RPC-002` | Auth implementation |
| `D-05` | SQLite или directory/segment store | INDEX-001 | Первая половина S3 | Persistent index |
| `D-06` | Persistent node/subresource identity | SCENE-001 | До конца S4 | Cross-revision node facts |
| `D-07` | GDScript LSP cache reuse vs independent analyzer adapter | SCRIPT-001 | До S5 acceptance | Symbol performance/correctness |
| `D-08` | Native history observation depth | EDITOR-001 | До S7 acceptance | Operation summaries |
| `D-09` | Runtime source mapping confidence | RUNTIME-001 | До S8 acceptance | Runtime evidence |
| `D-10` | Scene+script transaction atomicity boundary | WRITE-001 | До S10 | Full Undo claim |
| `D-11` | Windows Named Pipe adapter или secure loopback fallback | PLATFORM-001 | Spike после S2, freeze до S15 | Windows Beta |
| `D-12` | Evidence/runtime/transaction retention | SECURITY-001 | До Beta | Privacy/recovery |

Пропущенное решение переводит зависимую acceptance cell в `blocked`; реализация по случайному default не заменяет ADR/spec.

## 19. Артефакты по этапам

| Этап | Обязательные документы/артефакты |
|---|---|
| F0–F1 | ADR-001, PROTOCOL-001, SECURITY assumptions, module skeleton, conformance client |
| F2 | MCP-001 draft, schema bundle, R1-01 trace, live-editor fixture |
| F3 | INDEX-001, storage spike, resource golden graph, migration/rebuild report |
| F4 | SCENE-001, SCRIPT-001, EVIDENCE-001, semantic benchmark |
| F5 | EDITOR-001, overlay/gap/history traces |
| F6 | RUNTIME-001, runtime diagnostic evidence |
| F7 | WRITE-001, VALIDATION-001, fault matrix и transaction journal schema |
| F8 | SECURITY-001, PERFORMANCE-001, PLATFORM-001 и three-OS reports |

## 20. Источники и проверенные опоры

### 20.1. Документы проекта

- [MASTER_SPRINT_ROADMAP.md](MASTER_SPRINT_ROADMAP.md) — scope, архитектурный контур, Sprint 0–17 и gates.
- [PRODUCT-001-semantic-bridge-vision-and-plan.md](PRODUCT-001-semantic-bridge-vision-and-plan.md) — state layers, entity/fact model, parity и transactions.
- [RELEASE-001-release-1.0-acceptance-and-evidence-plan.md](RELEASE-001-release-1.0-acceptance-and-evidence-plan.md) — `R1-*`, evidence и release acceptance.

### 20.2. Godot source в текущем форке

Проверено на commit `2c089e9bf0b8712d0bc444c2ceaf9c543ed9c777`:

- `editor/editor_data.h` — scenes, selection, dirty/history state;
- `editor/docks/inspector_dock.h`, `editor/inspector/editor_inspector.h` — current Inspector object;
- `editor/file_system/editor_file_system.h` — filesystem/import state и signals;
- `core/io/resource_uid.h`, `core/io/resource_loader.h` — UID и dependencies;
- `scene/resources/packed_scene.h` — `SceneState` nodes, properties, groups, instances и connections;
- `modules/gdscript/gdscript_parser.h`, `gdscript_analyzer.h`, `language_server/gdscript_workspace.h` — GDScript semantic foundations;
- `editor/debugger/editor_debugger_node.h`, `script_editor_debugger.h` — runtime/debugger data;
- `editor/editor_undo_redo_manager.h` — editor-native histories и actions;
- `core/io/uds_server.h`, `stream_peer_uds.h` — Unix Domain Socket support;
- `drivers/windows/file_access_windows_pipe.*` — существующая Windows pipe abstraction, требующая отдельного suitability spike.

## 21. Критерии утверждения ARCHITECTURE-001

Документ можно перевести в `Approved`, когда:

- Engine owner подтверждает adapter и main-thread boundaries;
- Protocol owner принимает handshake, snapshot, sequence, resync и backpressure invariants;
- Sidecar/Index owner принимает normalization, staging/atomic activation и offline rules;
- Security reviewer принимает discovery/token/project isolation и logging policy;
- QA связывает `A1`–`A13` с fixture/test IDs и `R1-*` manifest;
- все `D-01`–`D-04` имеют владельцев и задачи до начала implementation;
- мастер-план и `PRODUCT-001` ссылаются на этот документ;
- ни одна дочерняя спецификация не создаёт второй editor access path, index или MCP semantics.
