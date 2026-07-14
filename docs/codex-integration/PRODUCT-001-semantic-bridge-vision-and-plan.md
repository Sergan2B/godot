# PRODUCT-001 — Единый семантический мост Godot × Codex

**Статус:** Approved 1.0

**Дата:** 2026-07-14

**Связанные документы:** [MASTER_SPRINT_ROADMAP.md](MASTER_SPRINT_ROADMAP.md), раздел 2; [ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md](ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md); [RELEASE-001-release-1.0-acceptance-and-evidence-plan.md](RELEASE-001-release-1.0-acceptance-and-evidence-plan.md)

**Целевой релиз:** 1.0

**Владельцы решения:** `Sergan2B` временно совмещает роли Product, Engine/Editor, Sidecar/Protocol, Codex Client и QA/Security до их делегирования

---

## 1. Назначение документа

Этот документ раскрывает продуктовое видение из раздела 2 мастер-плана и превращает его в проверяемую архитектурную модель и план поставки. Он отвечает на пять вопросов:

1. что означает «Codex понимает живой Godot-проект»;
2. какие состояния проекта существуют одновременно и какое из них авторитетно;
3. что именно является единым семантическим мостом;
4. как обеспечить одинаковый смысл ответов и операций во всех клиентах Codex;
5. какими этапами и доказательствами это видение доводится до релиза 1.0.

Документ является авторитетным для продуктовых инвариантов, слоёв состояния, требований к идентичности, revision/evidence model и паритету клиентских поверхностей. Он не заменяет детальные спецификации `PROTOCOL-001`, `MCP-001`, `INDEX-001`, `SCENE-001`, `EDITOR-001`, `RUNTIME-001` и `WRITE-001`.

При расхождении документов применяется следующий порядок:

1. мастер-план определяет обязательный scope, milestone и release gate;
2. этот документ определяет продуктовый смысл и сквозные инварианты;
3. утверждённые дочерние спецификации определяют wire format, API и реализацию;
4. несовместимость устраняется изменением документов и migration note, а не скрытым отклонением кода.

## 2. Проблема и продуктовый результат

Обычный файловый агент видит `.tscn`, `.tres`, `.gd` и `project.godot` как независимые тексты. Этого недостаточно, потому что значимая часть Godot-проекта существует только после интерпретации движком или редактором:

- путь ресурса может измениться, а его `ResourceUID` — сохраниться;
- итоговая сцена складывается из base scene, inheritance, instances и local overrides;
- открытая сцена может отличаться от версии на диске;
- выбранный объект Inspector может быть subresource или runtime object, а не строка в файле;
- динамическая связь GDScript может стать точной только во время исполнения;
- операция должна войти в корректную history Godot и иметь настоящий Undo path;
- после перезапуска игры прежний runtime object ID больше не описывает тот же объект.

Продуктовый результат 1.0:

> Любая поддерживаемая поверхность Codex получает один и тот же project-scoped, revision-aware и evidence-backed взгляд на статическое, редакторское и runtime-состояние Godot и выполняет семантические изменения через одну модель безопасных операций.

Пользователь не обязан вручную копировать scene tree, Inspector values, Output, stack trace или список signal connections в запрос. Codex сам получает минимально необходимый контекст, сообщает его источник и свежесть, а перед изменением показывает понятный preview.

## 3. Scope и non-goals

### 3.1. В scope 1.0

- ресурсы, `ResourceUID`, импорты и прямые/обратные зависимости;
- сцены, наследование, инстансы, subresources и local overrides;
- узлы, свойства, owner, groups, signals и animation `NodePath`;
- GDScript symbols/references и минимально доступная C#-семантика согласно мастер-плану;
- все открытые scene tabs, selection, Inspector и несохранённые изменения;
- состояние запуска, remote scene tree, runtime objects, diagnostics и stack traces;
- summary native Undo/Redo histories и полная история операций, созданных мостом;
- безопасные editor transactions, preview, approval, validation и Undo;
- общий MCP-контракт для Codex App, CLI, поддерживаемой IDE-интеграции и Godot Dock;
- offline/read-only режим при закрытом редакторе;
- project isolation, reconnect, recovery и capability negotiation.

### 3.2. Не входит в обязательный scope 1.0

- доказательство всех динамических GDScript-связей без runtime evidence;
- побайтовая реконструкция аргументов любой исторической операции, выполненной сторонним editor plugin;
- стабильная идентичность runtime object между разными запусками игры;
- совместное редактирование одной сцены несколькими редакторами;
- публичный сетевой доступ к локальному bridge;
- отдельная семантическая реализация под каждый Codex-клиент;
- полный Roslyn-паритет для C#;
- гарантия единого Undo для произвольных raw file edits, выполненных в обход semantic transaction;
- обязательный UI-паритет: одинаковым должен быть смысл контракта, а не внешний вид клиентов;
- обязательная поддержка автоматизаций и сторонних MCP-клиентов в 1.0. Архитектура обязана не блокировать их последующее подключение.

## 4. Термины

| Термин | Значение |
|---|---|
| Semantic bridge | Вся project-scoped цепочка Godot adapter → Bridge RPC → semantic state/index → MCP contract; это логическая граница, а не один класс или процесс |
| Bridge module | Editor-only `modules/codex_bridge` внутри Godot; единственный компонент, читающий и меняющий внутреннее editor state напрямую |
| Sidecar | Локальный `godot-codex-mcp`, который нормализует данные, хранит индекс и публикует MCP |
| Static state | Сохранённые файлы и производные данные, воспроизводимые без открытого редактора |
| Editor state | Живые сцены и объекты редактора, включая несохранённые изменения и UI context |
| Runtime state | Состояние конкретного запуска игры и debugger session |
| Operation state | Undo/Redo histories, bridge transactions и изменения revisions |
| Entity | Канонически идентифицируемый объект семантической модели |
| Fact | Утверждение об entity или связи, снабжённое evidence, confidence и revision |
| Snapshot | Согласованный срез нескольких слоёв состояния с revision vector |
| Overlay | Живые данные редактора, заменяющие соответствующую сохранённую проекцию для конкретной открытой сцены/ресурса |
| Surface | Клиент, из которого пользователь работает с Codex |
| Semantic parity | Эквивалентные entity IDs, facts, confidence, revisions, previews и errors при одинаковом запросе и состоянии проекта |

## 5. Продуктовые принципы и обязательные инварианты

### 5.1. Один смысл, несколько представлений

Каждый клиент использует общий MCP-контракт и общую семантическую модель. Клиент может по-разному визуализировать результат, но не должен иметь собственный Godot indexer, собственные entity IDs или скрытый путь записи.

### 5.2. Editor state не равно disk state

Ответ обязан различать сохранённую версию и live overlay. Если сцена открыта и dirty, запрос текущего состояния использует editor state, а evidence сообщает обе revisions и признак несохранённости.

### 5.3. Любой факт существует во времени

Ни один editor/runtime факт не возвращается без session/revision coordinates. Stale result не маскируется под текущий; изменение после начала запроса либо отражается новым snapshot, либо приводит к явному conflict/stale status.

### 5.4. Идентичность важнее пути

`res://` path и `NodePath` — адреса, но не универсальные постоянные идентификаторы. Ресурсы связываются прежде всего через UID; editor/runtime instances — через session-scoped IDs; fallback ID всегда сообщает область стабильности.

### 5.5. Evidence важнее уверенного текста

Каждый существенный semantic result содержит source, location, confidence и revision. Неоднозначная динамическая связь не получает `exact`.

### 5.6. Возможность принадлежит контракту, а не UI

Новая Godot capability сначала появляется в Bridge RPC и MCP schema с contract tests. После этого её могут использовать все поверхности. Прямая функция только в Godot Dock не считается частью общего моста.

### 5.7. Read доступен по умолчанию, write является транзакцией

Чтение не меняет проект. Любое bridge-managed изменение имеет preconditions, preview, approval policy, transaction ID, validation result и recovery/Undo path.

### 5.8. Один открытый проект — одна security boundary

Discovery, token, index, sessions, revisions и transactions изолированы по canonical project root. Наличие двух открытых редакторов не разрешает автоматический выбор «похожего» проекта.

### 5.9. Degraded mode является состоянием, а не догадкой

Закрытый editor, неактивный runtime, перестройка индекса, несовместимая версия и потерянное соединение представлены явными capabilities/status. Клиент не симулирует отсутствующий live context из старого cache без метки `stale`.

### 5.10. Семантический паритет проверяется автоматически

Одинаковые запросы на одном snapshot должны возвращать эквивалентные нормализованные результаты независимо от поверхности. Паритет доказывается contract/E2E tests, а не общей документацией клиентов.

## 6. Модель живого проекта

### 6.1. Четыре слоя состояния

| Слой | Авторитетный источник | Примеры | Идентификатор времени | Срок жизни |
|---|---|---|---|---|
| Static/index | `ResourceUID`, `ResourceLoader`, `EditorFileSystem`, `PackedScene`, parser/analyzer, файлы | UID, dependencies, scene definitions, symbols | `project_revision`, `index_revision`, content hash | Между сессиями; cache перестраиваем |
| Live editor | `EditorData`, `EditorSelection`, Inspector/editor objects, открытые script editors | dirty scenes, selection, unsaved property, active script | `editor_session_id`, `scene_revision`, `editor_event_seq` | До закрытия editor/scene; часть может быть сохранена |
| Runtime | `EditorDebugger` и remote scene tree | runtime nodes, properties, errors, stack frames, viewport | `runtime_session_id`, `runtime_event_seq` | Только один запуск игры |
| Operations | `EditorUndoRedoManager` и bridge transaction journal | undo/redo availability, action name, preview, apply/rollback | `history_id`, `operation_seq`, `transaction_id` | Native history — editor session; journal — по retention policy |

Слои не сводятся в один неразличимый cache. Sidecar хранит нормализованный граф и накладывает live/runtime projections только при наличии подходящих session/revision coordinates.

### 6.2. Revision vector

Минимальная координата согласованного ответа:

```json
{
  "project_id": "project:sha256:<canonical-root-fingerprint>",
  "project_revision": 1042,
  "index_revision": 877,
  "editor_session_id": "editor:01J...",
  "editor_event_seq": 19044,
  "scene_revisions": {
    "uid://player_scene": 83
  },
  "runtime_session_id": "runtime:01J...",
  "runtime_event_seq": 412,
  "operation_seq": 206
}
```

Поле отсутствует, если слой не запущен или не участвовал в ответе. `project_revision` монотонно меняется при любом принятом изменении индексируемого проекта. `scene_revision` меняется при live structural/property edit независимо от сохранения. Новый editor/game process всегда получает новый session ID.

[PROTOCOL-001](PROTOCOL-001-bridge-rpc-v1.md) фиксирует domain-separated SHA-256 `project_id` и session-scoped bridge counters для v1; `INDEX-001` позднее определяет persistent index revision/epoch. Наружу не должен попадать абсолютный путь, если он не нужен пользователю.

### 6.3. Правила наложения состояния

Для запроса «текущее состояние» действует следующий приоритет:

1. runtime projection — только если пользователь запрашивает runtime object или связь с конкретной активной runtime session;
2. live editor overlay — для открытой сцены/ресурса и editor context;
3. static semantic index — для сохранённой части проекта;
4. raw file observation — только как evidence или fallback с соответствующим source/confidence.

Правила merge:

- overlay заменяет только сущности/поля, которые явно покрывает;
- удалённый в dirty scene узел не «возвращается» из disk index;
- новый unsaved узел получает editor-session identity и до сохранения не объявляется постоянным;
- конфликт disk change и dirty overlay возвращается как две версии плюс diagnostic, а не разрешается молча;
- runtime property не заменяет editor default property: они являются разными facts, связанными отношением `runtime_instance_of`;
- cache после reconnect не становится текущим до full snapshot или подтверждённого event replay.

### 6.4. Уровни консистентности запроса

`MCP-001` должен поддержать как минимум два режима:

- `latest_available` — быстрый ответ с явным status/revision, допускающий `indexing` или `stale` части;
- `consistent_snapshot` — ответ только после согласования требуемых слоёв на зафиксированном revision vector либо structured timeout/conflict.

Write preconditions и validation всегда используют `consistent_snapshot`. Обычные обзорные запросы могут использовать `latest_available`.

## 7. Каноническая семантическая модель

### 7.1. Entity envelope

Каждая entity имеет одинаковую внешнюю оболочку:

```json
{
  "entity_id": "godot:node-definition:uid://player_scene#<opaque-node-key>",
  "kind": "node_definition",
  "display_name": "Player",
  "identity_scope": "persistent",
  "origin": {
    "resource_uid": "uid://player_scene",
    "path": "res://player/player.tscn",
    "node_path": "Player"
  },
  "revision": {
    "project_revision": 1042,
    "scene_revision": 83
  }
}
```

`entity_id` является opaque для клиентов: они могут хранить и передавать его, но не должны разбирать строку. `identity_scope` принимает `persistent`, `content_revision`, `editor_session` или `runtime_session`.

### 7.2. Основные entities и правила идентичности

| Entity | Предпочтительный ключ | Fallback и его ограничение |
|---|---|---|
| Project | canonical root fingerprint + project metadata | canonical path; не переносим между машинами |
| Resource/Scene/Script | `ResourceUID` | normalized `res://` path + content generation |
| Subresource | owner resource UID + stable subresource identifier | owner UID + serialized position/hash; invalidated on rewrite |
| Scene definition | scene resource UID | scene path + content generation |
| Node definition | scene UID + persistent scene node key, если Godot его предоставляет | scene UID + canonical `NodePath` + scene revision; меняется при rename/reparent |
| Editor node/object | `editor_session_id` + `ObjectID` | отсутствует; session-scoped по определению |
| Runtime object | `runtime_session_id` + remote object ID | отсутствует; не переносим между запусками |
| Script symbol | script UID + language adapter symbol key + content revision | file/range + content hash |
| Signal connection | source entity + signal + target entity + callable + declaration scope | content-revision scoped composite key |
| Group membership | node entity + group + declaration scope | content-revision scoped composite key |
| Diagnostic | source + code + location + observed revision | hash нормализованного сообщения; может измениться |
| Transaction | generated `transaction_id` | отсутствует |

До утверждения устойчивого node key `NodePath` нельзя объявлять persistent identity. Решение принимается после spike над `SceneState` в Sprint 3–4 и фиксируется в `SCENE-001`.

### 7.3. Основные связи

- `contains`;
- `references`;
- `instantiates`;
- `inherits`;
- `overrides`;
- `attaches_script`;
- `declares_symbol`;
- `references_symbol`;
- `connects_signal`;
- `belongs_to_group`;
- `preloads` / `loads`;
- `calls`;
- `runtime_instance_of`;
- `selected_in_editor`;
- `changed_by_transaction`;
- `validated_by`;
- `supersedes_revision`.

Relation vocabulary версионируется вместе с semantic schema. Client-specific relation names запрещены.

## 8. Facts, evidence и confidence

Semantic result состоит не из свободного текста, а из facts. Минимальная форма fact:

```json
{
  "fact_id": "fact:01J...",
  "subject": "godot:resource:uid://stats",
  "predicate": "referenced_by",
  "object": "godot:node-definition:uid://player#<opaque-node-key>",
  "evidence": [{
    "source": "live_editor_property",
    "path": "res://player/player.tscn",
    "node_path": "Player",
    "property": "stats",
    "line": null,
    "confidence": "exact",
    "freshness": "current",
    "scene_revision": 83
  }]
}
```

Обязательные правила:

- `exact` выдаётся только при структурном знании Godot или однозначном результате language adapter;
- `runtime-confirmed` относится к конкретной `runtime_session_id` и не превращается автоматически в статический `exact`;
- `probable` и `dynamic` сохраняются отдельно от exact results и доступны фильтрами;
- `stale` — статус свежести evidence, а не более слабая разновидность exactness;
- несколько источников одного fact агрегируются без потери provenance;
- противоречащие facts возвращаются вместе с conflict diagnostic;
- deduplication не объединяет одинаковый текст, если различаются entity IDs или revisions;
- summary для модели является проекцией facts, а не единственным носителем результата.

Авторитетность источника доменная: `ResourceUID` авторитетен для identity ресурса, live scene tree — для dirty scene, parser/analyzer — для разрешённого symbol reference, debugger — для runtime observation, Undo/Redo manager — для факта применённой editor operation.

## 9. Архитектура единого моста

Нормативные component boundaries, Bridge RPC lifecycle, index/evidence contract и foundation implementation plan определены в [ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md](ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md).

```text
Codex App ─────┐
Codex CLI ─────┼─────────────── project-scoped MCP ───────────────┐
Codex IDE ─────┘                                                   │
                                                                   ▼
Godot Codex Dock ── codex app-server ── Codex agent ──► godot-codex-mcp
                                                                   │
Future automation / third-party MCP client ────────────────────────┘
                                                                   │
                                             semantic index + overlay
                                                                   │
                                             versioned local Bridge RPC
                                                                   │
                                                                   ▼
                                                    modules/codex_bridge
                                                                   │
                    ResourceUID │ SceneState │ EditorData │ GDScript │
                    EditorDebugger │ EditorUndoRedoManager │ Project run
```

### 9.1. Что означает «один мост»

«Один» означает:

- одна canonical entity/relation/evidence model;
- один Bridge RPC для доступа к Godot internals;
- один MCP-контракт для model-facing capabilities;
- один project-scoped index и revision stream на editor instance;
- одна approval/transaction semantics;
- один набор contract fixtures и parity tests.

Это не означает один глобальный процесс для всех проектов. Каждый проект и editor instance имеет отдельную session/security boundary. Sidecar может обслуживать только тот project root, с которым был запущен и согласован.

### 9.2. Ответственность Bridge module

Bridge module:

- читает Godot internals на допустимом thread;
- выпускает bounded snapshots и ordered events;
- применяет editor-native операции через Undo/Redo;
- управляет run/debug commands в рамках editor lifecycle;
- проверяет session token и protocol version;
- не хранит model prompts, OpenAI credentials или Codex threads;
- не формирует client-specific тексты и UI;
- не выполняет тяжёлый полнотекстовый/графовый поиск в main thread.

### 9.3. Ответственность sidecar

Sidecar:

- обнаруживает только согласованный editor/project instance;
- нормализует snapshots/events в semantic graph;
- хранит перестраиваемый индекс и overlays;
- реализует query, pagination, evidence aggregation и token-bounded summaries;
- публикует MCP tools/resources/instructions;
- координирует semantic transactions и approvals;
- поддерживает offline read-only cache;
- не меняет открытую сцену raw file write;
- не становится вторым редактором Godot.

### 9.4. Роль Codex app-server

`codex app-server` обслуживает встроенный клиент: authentication, threads, turns, streaming, approvals и agent items. Он не заменяет Godot MCP и не является источником Godot-семантики. Godot Dock запускает или обнаруживает app-server, а выполняемый им Codex использует тот же project-scoped MCP, что App/CLI/IDE.

## 10. Общий model-facing контракт

### 10.1. Слои контракта

1. **Status/capabilities:** project binding, versions, available layers, degraded mode.
2. **Entities/query:** overview, inspection, graph traversal, find usages.
3. **Editor context:** open scenes, selection, Inspector, scripts, diagnostics.
4. **Runtime:** lifecycle, remote tree, objects, stack, screenshot.
5. **Operations:** plan, preview, approve, apply, validate, undo.
6. **Resources:** stable, cacheable summaries/snapshots для model context.

Предварительные имена tools находятся в разделе 6 мастер-плана. `MCP-001` должен нормализовать общие поля каждого ответа:

- `schema_version`;
- `project_id`;
- `capabilities_used`;
- `revision_vector`;
- `status` и `partial_reasons`;
- `entities` / `facts` / `evidence`;
- `diagnostics`;
- `next_cursor`;
- `limits_applied`.

### 10.2. Capability negotiation

Handshake bridge и MCP обязан различать как минимум:

- schema/protocol version;
- Godot version/fork capability set;
- static index readiness;
- live editor availability;
- runtime/debugger availability;
- supported languages;
- read/write/transaction capabilities;
- screenshot capability;
- platform transport capability;
- experimental features.

Отсутствующая capability возвращает `capability_unavailable`, а не пустой успешный результат.

### 10.3. Surface parity

| Поверхность | Путь к семантике | Обязательность в 1.0 | Особенность UX |
|---|---|---:|---|
| Codex App | Локальный project-scoped MCP | Да | Основной внешний интерактивный клиент |
| Codex CLI | Тот же MCP/config | Да | Terminal-first output и approvals |
| Поддерживаемая IDE-интеграция | Тот же MCP/config | Да | IDE-attached context, но без отдельного Godot index |
| Godot Codex Dock | app-server для Codex lifecycle + тот же MCP для Godot | Да | Semantic preview и Undo controls внутри Godot |
| Automation/SDK | Тот же MCP или будущий утверждённый adapter | Нет, архитектурная готовность | Noninteractive policy и явные approvals |
| Сторонний MCP client | Стандартный MCP с capability checks | Нет, архитектурная готовность | Не гарантируется Codex-specific UX |

Паритет считается достигнутым, если при одинаковом snapshot и параметрах совпадают:

- entity IDs и graph facts;
- confidence/evidence и revision vector;
- normalized errors/status;
- transaction preconditions и semantic preview;
- approval classification;
- post-apply validation result.

Форматирование, сортировка UI, streaming presentation и способ навигации могут различаться.

## 11. Основные data flows

### 11.1. Подключение и начальная синхронизация

1. Bridge создаёт защищённый discovery record и новую editor session.
2. Sidecar проверяет canonical project root, token и версии.
3. Стороны согласуют capabilities и last known revision.
4. При совместимом cache sidecar запрашивает delta; иначе — bounded full snapshot.
5. Sidecar применяет snapshot атомарно и объявляет readiness.
6. MCP status становится `ready`, `partial` или `offline` с причинами.

Старый discovery record не даёт доступа к новой сессии. После gap в event sequence delta stream не продолжается без resync.

### 11.2. Read query

1. MCP валидирует project binding, query limits и consistency mode.
2. Query planner определяет необходимые static/editor/runtime sources.
3. Sidecar фиксирует revision vector или сообщает partial/stale состояние.
4. Индекс и overlays возвращают entities/facts.
5. Evidence aggregator выполняет deduplication без потери provenance.
6. Результат ограничивается pagination/output budget.
7. Model-facing summary строится из тех же facts и содержит ссылки на evidence IDs.

### 11.3. Live editor update

1. Bridge замечает поддерживаемое изменение и увеличивает scene/editor sequence.
2. Событие содержит минимальный delta и affected entity hints.
3. Sidecar проверяет непрерывность sequence и precondition revision.
4. Overlay и зависимые index edges обновляются атомарно.
5. Активные prepared transactions, чьи preconditions затронуты, становятся `conflicted`.

Event stream является механизмом инвалидации, но не единственным источником восстановления. Full snapshot остаётся обязательным recovery primitive.

### 11.4. Runtime observation

1. Команда запуска создаёт новую `runtime_session_id`.
2. Debugger events нормализуются отдельно от editor entities.
3. Runtime objects связываются с source scene/node только при наличии evidence.
4. Pause/restart/disconnect изменяют runtime status и sequence.
5. После завершения session runtime entities остаются доступны только как stale diagnostic record по retention policy и не считаются живыми.

### 11.5. Semantic transaction

Обязательный lifecycle:

```text
draft → planned → previewed → awaiting_approval → applying
      → applied → validating → committed → undone
                         └────→ validation_failed → rolled_back/needs_attention
```

Каждый change set содержит:

- project/editor/scene revision preconditions;
- ordered semantic operations;
- affected entities и files;
- risk/approval classification;
- human-readable semantic preview;
- machine-readable inverse/rollback data;
- validation plan;
- transaction ID и idempotency key.

Scene/resource operations применяет Bridge module через `EditorUndoRedoManager`. Составное изменение scene + script считается полностью undoable только если script patch также прошёл через semantic change set. Для такого patch сохраняются guarded before/after hashes, а inverse operation регистрируется в согласованной history. Произвольный raw file edit вне моста вызывает reindex и diagnostic, но не получает ложную гарантию единого Undo.

После apply sidecar ждёт новые revisions, выполняет diagnostics/optional run validation и сравнивает pre/post graph. Несовпадение ожидаемого revision, частичный apply или потеря связи приводит к recovery flow, а не к автоматическому повтору destructive command.

### 11.6. История операций

В 1.0 поддерживаются два уровня наблюдаемости:

1. **Bridge transactions:** полный plan/preview/operations/result/validation/undo status.
2. **Native editor actions:** history ID, action name, saved/unsaved version, наличие Undo/Redo, sequence и affected scene при безопасном определении.

Мост не обещает восстановить все аргументы произвольной операции стороннего plugin. Если payload недоступен, действие всё равно инвалидирует revisions и отображается как opaque native action.

## 12. Ошибки, reconnect и recovery

Обязательные error classes:

| Код | Смысл | Ожидаемое действие клиента |
|---|---|---|
| `project_not_bound` | Sidecar не доказал соответствие project root | Остановить запрос, запустить setup/doctor |
| `editor_offline` | Нет live editor | Предложить offline read-only result, если он допустим |
| `index_not_ready` | Initial build/migration/rebuild | Показать progress/retry hint |
| `runtime_not_active` | Runtime tool вызван без session | Предложить запуск, не возвращать старое дерево как текущее |
| `stale_revision` | Preconditions устарели | Перечитать snapshot и построить новый preview |
| `snapshot_conflict` | Слои изменились во время consistent query | Retry с bounded policy или вернуть конфликт |
| `capability_unavailable` | Версия/платформа не поддерживает функцию | Показать capability reason |
| `approval_required` | Write не подтверждён | Передать structured preview в surface |
| `transaction_in_doubt` | Связь потеряна во время apply | Запросить status по transaction ID; не повторять вслепую |
| `protocol_mismatch` | Несовместимые версии | Doctor/remediation, без частичной записи |
| `overloaded` | Очередь/лимит исчерпан | Backoff с jitter и cancellation support |
| `result_truncated` | Применён size/token limit | Продолжить по cursor или сузить запрос |

Recovery rules:

- event sequence gap всегда вызывает resync;
- corrupt/mismatched cache изолируется и перестраивается;
- editor или sidecar crash не удаляет project files и не auto-commits prepared transaction;
- transaction status запрашивается по ID после reconnect;
- `prepared` без apply безопасно истекает;
- `applying` после disconnect считается `in_doubt` до сверки editor history и revisions;
- recovery никогда не применяет write повторно только потому, что клиент не получил response.

## 13. Безопасность и приватность

- Bridge доступен только локально через UDS/Named Pipe или защищённый loopback fallback.
- Discovery и capability token имеют user-only permissions и не попадают в Git.
- Token не передаётся в MCP output, model context, CLI args или обычные logs.
- Absolute paths, environment variables, editor output и Variant values проходят redaction/size policy.
- Каждый request проверяет project/session binding; sidecar одного workspace не перечисляет другие проекты.
- Read tools маркируются как read-only; write/destructive tools получают корректные MCP approval hints.
- Third-party MCP client не получает менее строгую approval policy, чем Codex surfaces.
- Runtime inspection ограничивает глубину, количество объектов, типы Variant и screenshot rate.
- Bridge не содержит OpenAI API keys и не управляет Codex account.
- Godot Dock не обходится без app-server/Codex auth только потому, что находится внутри редактора.
- Transaction journal хранит структуру и hashes; чувствительные полные значения сохраняются только когда это необходимо для Undo и под отдельной retention policy.

Полная threat model фиксируется в `SECURITY-001`; security work начинается со Sprint 1, а не откладывается до hardening.

## 14. Производительность и ресурсные ограничения

Сквозные бюджеты наследуются из раздела 8 мастер-плана. Дополнительно архитектура обязана обеспечить:

- bounded snapshot chunks вместо неограниченной сериализации scene tree;
- main-thread time slicing для editor introspection;
- background index storage/query вне editor process;
- coalescing высокочастотных property events без потери итоговой revision;
- backpressure и cancellation на каждом transport boundary;
- pagination для graph/query/runtime tree;
- lazy serialization тяжёлых Variant/resources;
- separate priority для status/selection и bulk indexing;
- измеряемое время от editor change до query visibility;
- отсутствие повторного полного index build при обычном reconnect.

Начальные SLO до калибровки Sprint 3/14:

| Операция | Цель |
|---|---:|
| Cached semantic query p95 | ≤ 300 мс |
| Current scene/selection p95 | ≤ 500 мс |
| Видимость изменённого файла в индексе p95 | ≤ 2 с |
| Status/ping при bulk indexing p95 | ≤ 200 мс |
| Обнаружение scene revision conflict | До начала apply |
| Необработанные editor stalls сверх frame budget | 0 на reference fixtures |

## 15. План реализации

План ниже не создаёт второй календарь. Он группирует semantic-bridge work и явно сопоставляет его со Sprint 0–17 мастер-плана.

### 15.1. Этапы поставки

| Этап | Спринты | Результат | Основные артефакты | Exit criteria |
|---|---:|---|---|---|
| SB-0 Product contract | S0–S1 | Утверждены инварианты, границы и термины | `PRODUCT-001`, ADR-001, schema conventions, fixture taxonomy | Нет открытых решений, блокирующих protocol skeleton |
| SB-1 Connected skeleton | S1–S2 | Живой editor context проходит end-to-end | `PROTOCOL-001`, MCP draft, bridge/sidecar skeleton | Unsaved selected property виден внешнему Codex с revision |
| SB-2 Static semantic core | S3–S6 | Ресурсы, сцены и scripts образуют evidence-backed graph | `INDEX-001`, `SCENE-001`, `SCRIPT-001`, `EVIDENCE-001` | Find usages, zero false `exact`, rebuild/migration proven |
| SB-3 Live overlay and history | S7 | Все открытые сцены и native action summaries согласованы с disk index | `EDITOR-001`, history event contract | Dirty overlay побеждает disk; gaps/revisions/conflicts проверены |
| SB-4 Runtime projection | S8 | Конкретная game session наблюдаема и связана с source entities | `RUNTIME-001` | Runtime tree, error, stack и session reset проходят E2E |
| SB-5 Safe semantic writes | S9–S10 | Change sets имеют preview, approval, validation и Undo | `WRITE-001`, `VALIDATION-001` | Atomic apply/rollback, stale rejection, full Undo доказаны |
| SB-6 Surface parity | S11–S13 | App, CLI, IDE и Dock используют один contract | final `MCP-001`, `UI-001`, parity harness | Нормализованные сценарии дают эквивалентный результат |
| SB-7 Production readiness | S14–S17 | Мост безопасен, быстр и переносим | SECURITY/PERFORMANCE/PLATFORM/TEST/RELEASE specs | Alpha/Beta/RC/Stable gates мастер-плана пройдены |

### 15.2. Workstreams

#### WS-1 — Identity, schema и revisions

- определить opaque entity ID envelope;
- провести spike по persistent node identity и subresource identity;
- определить revision vector и event sequence rules;
- зафиксировать schema versioning/migration policy;
- реализовать canonical normalization для parity tests.

**Зависимости:** начинается в S1; node decision закрывается до завершения S4.

**Доказательство:** rename/reparent/save/reopen fixtures сохраняют identity только там, где это обещано scope.

#### WS-2 — Ingestion и semantic graph

- resource/UID/dependency adapter;
- PackedScene/inheritance/instance adapter;
- GDScript/C# language adapters;
- incremental queue, invalidation и rebuild;
- live overlay ingestion;
- runtime projection ingestion.

**Зависимости:** transport skeleton; entity schema.

**Доказательство:** golden graph и mutation fixtures.

#### WS-3 — Query, evidence и context shaping

- graph queries и unified find usages;
- evidence aggregation/conflict representation;
- filters, pagination, limits и cursors;
- token-bounded summaries, выведенные из facts;
- consistency modes и partial-result rules.

**Зависимости:** WS-1/2.

**Доказательство:** semantic benchmark и zero false `exact` на fixtures.

#### WS-4 — Editor context и operation observation

- open scene tabs, selection, Inspector, active script;
- dirty markers и scene revisions;
- native Undo/Redo summary events;
- full snapshot/event replay/reconnect;
- safe Variant serialization.

**Зависимости:** evidence/revision model.

**Доказательство:** manual edits, undo, redo, save, external disk change и reconnect scenarios.

#### WS-5 — Runtime и diagnosis

- run/pause/continue/stop lifecycle;
- remote tree/object/stack/diagnostics;
- source mapping с evidence;
- screenshot capability и limits;
- crash/timeout/disconnect semantics.

**Зависимости:** editor entity model.

**Доказательство:** deliberate error/crash fixtures и новый session ID на каждый run.

#### WS-6 — Transactions и validation

- change-set schema, preconditions и idempotency;
- semantic preview и approval classification;
- editor-native apply/Undo;
- guarded script/resource changes;
- validation/rollback и in-doubt recovery.

**Зависимости:** live revisions, runtime diagnostics.

**Доказательство:** fault injection на каждой границе lifecycle.

#### WS-7 — MCP и client parity

- common tools/resources/instructions;
- project-scoped setup и doctor;
- canonical response comparator;
- App/CLI/IDE E2E harness;
- app-server adapter и Godot Dock;
- approval/diff/semantic preview presentation.

**Зависимости:** стабильный read/write contract.

**Доказательство:** один fixture trace воспроизводится на четырёх обязательных поверхностях.

#### WS-8 — Security, performance и operations

- token/discovery permissions и isolation;
- redaction, size limits, backpressure;
- benchmark projects и telemetry без project content;
- cross-platform transport/package/update;
- compatibility matrix и release evidence package.

**Зависимости:** начинается в S1 и сопровождает все этапы.

**Доказательство:** threat model, benchmark, recovery и install reports.

### 15.3. Критический путь и допустимый параллелизм

Критический путь semantic bridge:

```text
identity/revisions → bridge handshake → first live slice
→ resource graph → scene graph → script graph → evidence/query
→ live overlay ─┬→ runtime ─────┐
               └→ transactions ┴→ validation
→ external parity → embedded parity → hardening/platform/release
```

Параллельно допустимы:

- protocol schema и fixture design после утверждения общих терминов;
- scene fixtures во время resource index work;
- GDScript adapter после фиксации entity envelope;
- runtime и basic transaction foundations после live overlay;
- Dock UI на mock contract после semantic alpha;
- Windows/Linux transport prototypes после первого macOS vertical slice;
- threat modeling, documentation и performance instrumentation на всём пути.

Нельзя принимать в обход зависимостей:

- persistent node ID до проверки его поведения на inheritance/instances;
- live overlay без revision/conflict model;
- write operation без точного project/editor binding;
- embedded-only semantic capability;
- surface parity по snapshot, полученному в разное время;
- release gate без сохранённого evidence package.

### 15.4. Ближайший исполнимый backlog

После обязательных Sprint 0 артефактов из мастер-плана:

1. Утвердить `PRODUCT-001` и назначить владельцев сквозных инвариантов.
2. [Выполнено] В [ADR-001](ADR-001-component-boundaries-and-sidecar-language.md) зафиксировать component boundaries и язык sidecar.
3. [Выполнено] В [PROTOCOL-001](PROTOCOL-001-bridge-rpc-v1.md) определить handshake, project binding, session-scoped revision envelope, errors и framing.
4. Создать fixture taxonomy: resource rename, inherited scene, dirty overlay, runtime error, transaction fault.
5. Создать protocol conformance client и canonical JSON normalizer.
6. Реализовать `initialize/ping/capabilities/shutdown` без MCP.
7. Реализовать первый MCP slice: editor status, current scene, selection, unsaved property.
8. Заархивировать trace Godot → Bridge RPC → sidecar → MCP → Codex как M0 evidence.

## 16. Тестовая стратегия и acceptance matrix

### 16.1. Уровни тестирования

- unit tests для adapters, identity, revisions, merge и redaction;
- schema/contract tests для Bridge RPC и MCP;
- golden graph tests для resources/scenes/scripts;
- integration tests bridge ↔ sidecar без модели;
- parity tests поверх normalized MCP responses;
- E2E tests с реальным editor/runtime/process lifecycle;
- fault-injection tests для apply, disconnect, crash и corrupt cache;
- manual acceptance только там, где UX или platform packaging нельзя доказать ниже.

### 16.2. Сквозные acceptance scenarios PRODUCT-001

| ID | Сценарий | Что доказывает | Минимальное evidence |
|---|---|---|---|
| PV-01 | Ресурс переименован, UID сохранён, reverse usages обновлены | Resource identity не равна path | Golden graph до/после + revision trace |
| PV-02 | Inherited scene с instance и local override объяснена без чтения порядка `.tscn` | Scene semantics | Scene graph snapshot + exact evidence |
| PV-03 | Выбран узел, свойство изменено без save | Live overlay | Disk/editor values, dirty flag, scene revision |
| PV-04 | Symbol usage найден в script и связан с scene node | Cross-domain graph | File/range + attached-script edge |
| PV-05 | Dirty scene конфликтует с внешним disk change | Conflict model | Оба revisions + structured diagnostic |
| PV-06 | Новый run создаёт новый runtime session; error связан с source | Runtime projection | Tree, stack, source evidence, session IDs |
| PV-07 | Manual editor action/Undo/Redo меняют operation и scene revisions | Operation observation | History summary + ordered events |
| PV-08 | Compound semantic change применяется, валидируется и полностью отменяется | Transaction safety | Preview, approval, transaction journal, pre/post graph |
| PV-09 | App, CLI, IDE и Dock выполняют один trace | Surface parity | Canonical response diff без semantic differences |
| PV-10 | Два проекта открыты одновременно | Isolation | Отрицательные cross-project access tests |
| PV-11 | Sidecar падает во время read и prepared write | Recovery | Reconnect/resync; no project mutation |
| PV-12 | Editor закрыт, совместимый cache существует | Honest degraded mode | Offline capabilities, stale/fresh labels, write denial |
| PV-13 | Clean install, update, doctor и uninstall выполняются на macOS, Windows и Linux | Platform operability и самостоятельная эксплуатация | Platform E2E logs, doctor reports, artifact hashes и signed manual checklist |

### 16.3. Трассировка исходного видения

| Пункт раздела 2 мастер-плана | Покрытие |
|---|---|
| Ресурсы и UID | PV-01, WS-1/2 |
| Сцены, наследование и инстансы | PV-02, WS-2 |
| Узлы, свойства, группы и сигналы | PV-02/03, WS-2/4 |
| Скрипты и программные символы | PV-04, WS-2/3 |
| Открытый редактор и несохранённые изменения | PV-03/05, WS-4 |
| Runtime tree, errors и stack | PV-06, WS-5 |
| История, Undo/Redo и editor operations | PV-07/08, WS-4/6 |
| Общий мост для всех поверхностей | PV-09/10/12, WS-7/8 |
| Установка, update, recovery и uninstall на трёх ОС | PV-11/13, WS-8 |

## 17. Метрики успеха

Помимо release metrics мастер-плана собираются:

- доля semantic answers с полным revision vector;
- число ложных `exact` — обязательная цель 0 на golden fixtures;
- доля resolvable entities с persistent identity;
- p50/p95 editor-change-to-query-visible latency;
- частота full resync и причины event gaps;
- cache hit и rebuild rate;
- число surface parity divergences на contract suite;
- transaction conflict, validation failure, rollback и in-doubt rates;
- editor main-thread time per snapshot/event batch;
- объём redacted/truncated fields без сохранения содержимого проекта;
- recovery success rate после принудительного process crash.

Метрика не может собирать содержимое проекта без отдельного opt-in. Release report фиксирует fixture/benchmark versions и точные версии всех компонентов.

## 18. Риски и решения

| Риск | Проявление | Контроль |
|---|---|---|
| Node identity нестабилен | Usage/preview ссылается не на тот узел после rename | Opaque IDs, честный scope, spike и revision fallback |
| Live/disk merge скрывает удаление | Codex видит «призрачный» узел | Tombstones в overlay и conflict fixtures |
| Event loss создаёт тихо stale index | Неверный current state | Sequence gap → mandatory resync |
| Runtime mapping слишком уверен | Исправляется не тот source node | Evidence/confidence и session scoping |
| Native history не раскрывает payload | Нельзя объяснить стороннее действие | Summary contract; full detail только bridge transactions |
| Scene + script change не атомарен | Partial Undo | Semantic change set, guarded hashes, fault injection |
| Клиенты расходятся | Разные answers/approvals | Один MCP, canonical comparator, parity gate |
| Большой snapshot подвешивает editor | Потеря UX | Chunking, time slicing, limits, priority queues |
| Старый discovery/token переиспользован | Cross-session data leak | Session-bound token, permissions, expiry |
| App-server и MCP version drift | Dock ломается отдельно | Generated schemas, pinned compatibility, doctor |

## 19. Открытые решения и дедлайны

Решения о языке/packaging sidecar, Bridge RPC encoding/authentication, project fingerprint и session-scoped revisions закрыты [ADR-001](ADR-001-component-boundaries-and-sidecar-language.md) и [PROTOCOL-001](PROTOCOL-001-bridge-rpc-v1.md) 2026-07-14. Persistent index epoch остаётся частью будущего `INDEX-001`.

| Решение | Где фиксируется | Не позднее |
|---|---|---:|
| Язык и packaging sidecar | ADR-001 | До реализации S1 sidecar skeleton |
| Bridge RPC encoding/transport schema | PROTOCOL-001 | До завершения S1 |
| Project fingerprint и revision persistence | PROTOCOL-001 / INDEX-001 | До S2 acceptance |
| Storage engine/index migration | INDEX-001 | Первая половина S3 |
| Persistent node/subresource identity | SCENE-001 | До завершения S4 |
| Глубина native history observation | EDITOR-001 | До завершения S7 |
| Runtime-to-source mapping confidence | RUNTIME-001 | До завершения S8 |
| Atomicity scene + script change | WRITE-001 | До начала S10 |
| MCP resources/tools/approval annotations | MCP-001 | Alpha draft S6, freeze S11/S16 |
| App-server version/support policy для Dock | UI-001 | До S12 integration |
| Retention transaction/runtime evidence | SECURITY-001 | До Beta gate |
| Automation/third-party support tier | Roadmap 1.1 | После стабильного MCP v1 |

Открытое решение не должно маскироваться реализацией по умолчанию. Если дедлайн не выполнен, зависимый acceptance criterion блокируется.

## 20. Карта дочерних документов

| Документ | Что он обязан конкретизировать из PRODUCT-001 |
|---|---|
| [ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md](ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md) | Сквозные component boundaries, lifecycle, revisions/snapshots, index/evidence contract и implementation backlog |
| [ADR-001](ADR-001-component-boundaries-and-sidecar-language.md) | Process/component boundaries и запрет параллельных semantic implementations |
| [PROTOCOL-001](PROTOCOL-001-bridge-rpc-v1.md) | Handshake, token, framing, session-scoped revisions, lifecycle errors и compatibility |
| MCP-001 | Общая query/operation schema, resources, approvals, capability mapping |
| INDEX-001 | Entity/relation storage, revisions, migrations, invalidation |
| SCENE-001 | Scene/node identity, inheritance, instances, overrides, connections |
| SCRIPT-001 | Symbol identity, references, dynamic confidence, C# boundary |
| EVIDENCE-001 | Fact/evidence schema, source taxonomy, conflicts, deduplication |
| EDITOR-001 | Open tabs, selection, Inspector, overlays, history summaries |
| RUNTIME-001 | Runtime sessions, remote objects, stacks, source mapping |
| WRITE-001 | Change sets, preview, approvals, editor history и Undo |
| VALIDATION-001 | Pre/post graph, diagnostics, run checks, rollback policy |
| UI-001 | app-server lifecycle и представление общего MCP contract в Dock |
| SECURITY-001 | Trust boundaries, retention, redaction, third-party clients |
| PERFORMANCE-001 | Budgets, benchmark projects, event/snapshot profiling |
| TEST-001 | Fixtures PV-01–PV-13, canonical comparator, gate evidence |
| PLATFORM-001 | UDS/Named Pipe, lifecycle и packaging на трёх ОС |
| [RELEASE-001-release-1.0-acceptance-and-evidence-plan.md](RELEASE-001-release-1.0-acceptance-and-evidence-plan.md) | Requirements `R1-*`, pass criteria, compatibility matrix, migrations, support и evidence archive |

## 21. Источники и проверенные опоры

### 21.1. Документы репозитория

- [MASTER_SPRINT_ROADMAP.md](MASTER_SPRINT_ROADMAP.md) — scope 1.0, архитектурный контур, спринты, release gates и acceptance scenarios.
- [ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md](ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md) — техническая декомпозиция bridge/sidecar, Bridge RPC, индекс, evidence и foundation plan.
- [RELEASE-001-release-1.0-acceptance-and-evidence-plan.md](RELEASE-001-release-1.0-acceptance-and-evidence-plan.md) — нормативная трассировка десяти обязательных сценариев, метрик, gates и evidence.

### 21.2. Godot source в текущем форке

- `core/io/resource_uid.h` — `ResourceUID` и UID/path mapping;
- `scene/resources/packed_scene.h` — `SceneState`, nodes, groups, connections, base scene и instance data;
- `editor/editor_data.h` — открытые сцены, live scene roots, dirty state, selection и history IDs;
- `editor/editor_undo_redo_manager.h` — histories, actions, saved versions, Undo/Redo;
- `editor/debugger/editor_debugger_node.h` — debugger lifecycle и remote scene/object flow;
- `modules/gdscript/gdscript_parser.h` и `gdscript_analyzer.h` — parser/analyzer foundation.

Эти файлы подтверждают доступность базовых внутренних опор, но не являются стабильным public API. Bridge обязан изолировать их узким adapter layer.

### 21.3. Официальная документация Codex

- [Model Context Protocol](https://learn.chatgpt.com/docs/extend/mcp) — локальные Codex-клиенты поддерживают MCP; desktop app, CLI и IDE extension разделяют MCP configuration на одном host.
- [Codex App Server](https://learn.chatgpt.com/docs/app-server) — app-server предназначен для deep integration с authentication, threads, approvals и streamed events; schemas генерируются для конкретной версии Codex.

Контрактные предположения проверены 2026-07-14. Актуальность повторно проверяется при S11, S12, S16 и Stable gate, как требует мастер-план.

## 22. Критерии утверждения PRODUCT-001

Документ можно перевести из Draft в Approved, когда:

- Product подтверждает scope/non-goals и смысл semantic parity;
- Engine owner подтверждает реализуемость четырёх state layers и adapter boundaries;
- Protocol owner принимает revision, event и recovery invariants;
- Client owner подтверждает разделение MCP и app-server;
- QA принимает PV-01–PV-13 как основу `TEST-001`;
- Security reviewer принимает project/session boundary и отсутствие public listener;
- все решения с дедлайном до S1 либо закрыты, либо имеют владельца и spike task;
- мастер-план содержит ссылку на этот документ.
