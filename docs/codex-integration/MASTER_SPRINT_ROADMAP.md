# Godot × Codex Integration — мастер-план спринтов до релиза 1.0

**Статус:** Review candidate 1.0
**Дата:** 2026-07-14
**Базовый репозиторий:** `Sergan2B/godot`
**Базовый коммит на момент создания документа:** `2c089e9bf0b8712d0bc444c2ceaf9c543ed9c777`
**Базовая версия движка:** Godot 4.8-dev
**Основная платформа разработки:** macOS
**Целевые платформы релиза 1.0:** macOS, Windows, Linux
**Базовая длительность спринта:** 2 недели, кроме Sprint 0

---

## 1. Назначение документа

Этот документ задаёт полный порядок разработки интеграции Godot и Codex: от подготовки форка движка до стабильного релиза 1.0. Он определяет границы продукта, архитектурные блоки, последовательность спринтов, результаты каждого спринта, критерии приёмки и release gates.

Документ намеренно не описывает реализацию каждой подсистемы до уровня классов и методов. Для каждого крупного направления предполагается отдельная спецификация. Мастер-план отвечает на вопросы:

- что и в каком порядке разрабатывается;
- почему выбран именно такой порядок;
- какие зависимости существуют между блоками;
- какой проверяемый результат обязан дать каждый спринт;
- когда продукт считается alpha, beta, release candidate и stable;
- что именно означает «полностью работоспособная интеграция» в рамках версии 1.0.

Карта покрытия вопросов мастер-плана:

| Вопрос | Авторитетный раздел документа |
|---|---|
| Что входит и не входит в продукт 1.0 | 2–4 |
| Из каких архитектурных блоков он состоит | 5–6 |
| Что и в каком порядке разрабатывается | 9–10 |
| Почему выбран такой порядок | 9.1 и 12 |
| Какие зависимости существуют между блоками и спринтами | 5 и 12 |
| Какой результат обязан дать каждый спринт | Артефакты и критерии приёмки S0–S17 в разделе 9 |
| Когда продукт считается Alpha, Beta, RC и Stable | 11 |
| Что означает полностью работоспособная версия 1.0 | 3, 8, 11 и 13; детальная приёмка — `RELEASE-001` |

## 2. Видение продукта

Codex должен понимать Godot-проект не только как набор текстовых файлов, но как живую систему, состоящую из:

1. ресурсов и их UID;
2. сцен, наследования и инстансов;
3. узлов, свойств, групп и сигналов;
4. скриптов и программных символов;
5. открытого состояния редактора, включая несохранённые изменения;
6. запущенной игры, runtime-дерева, ошибок и стека вызовов;
7. истории изменений, Undo/Redo и операций редактора.

Один общий семантический мост должен обслуживать все поверхности Codex:

- отдельное приложение Codex;
- Codex CLI;
- IDE-интеграции Codex;
- встроенную панель Codex внутри Godot;
- будущие автоматизации и сторонние MCP-клиенты.

Подробная продуктовая модель, сквозные инварианты, требования к паритету поверхностей и план поставки этого моста определены в [PRODUCT-001-semantic-bridge-vision-and-plan.md](PRODUCT-001-semantic-bridge-vision-and-plan.md).

## 3. Определение релиза 1.0

Версия 1.0 считается завершённой, если пользователь может открыть игровой проект в специальной сборке Godot, открыть тот же проект в Codex и выполнить следующие сценарии без ручной передачи контекста:

1. Спросить, какая сцена и какой узел сейчас выбраны, включая несохранённые свойства.
2. Найти использования ресурса, сцены, узла, сигнала или GDScript-символа с доказательствами.
3. Получить объяснение структуры текущей сцены и её зависимостей.
4. Запустить проект, получить runtime-дерево, ошибки, stack trace и снимок viewport.
5. Попросить Codex диагностировать проблему, сопоставив файлы, редактор и runtime.
6. Попросить Codex изменить сцену через безопасную транзакцию редактора.
7. Просмотреть preview, подтвердить изменение, применить его и отменить через Undo.
8. Выполнить те же действия из Codex App, Codex CLI, поддерживаемой IDE-интеграции и встроенной панели Godot.
9. Перезапустить Godot или MCP-sidecar и продолжить работу без повреждения проекта.
10. Установить интеграцию на macOS, Windows или Linux по документированной процедуре.

Все десять сценариев являются обязательными и трактуются совместно с метриками раздела 8 и release gates раздела 11. «Полностью работоспособная интеграция» означает, что:

- пользователю не требуется вручную копировать scene tree, логи, stack trace или выбранные Inspector-значения в запрос;
- файловое состояние, несохранённое editor state и runtime state различаются и имеют revision/session identifiers;
- утверждения о проекте сопровождаются evidence и корректным confidence level;
- изменения сцен выполняются через редактор, имеют preview, approval, validation и полный Undo path;
- Codex App, CLI, поддерживаемая IDE-интеграция и встроенный клиент используют один и тот же MCP-контракт и дают эквивалентный семантический результат;
- сбой или перезапуск любого вспомогательного процесса не повреждает проект и имеет документированный recovery path;
- установка, обновление, диагностика и удаление проверены на всех трёх целевых ОС;
- обязательный сценарий нельзя объявить выполненным только на основании unit test: требуется соответствующий integration/E2E или manual acceptance evidence.

Known limitation может уточнять границы динамического анализа или UX, но не может отменять один из обязательных сценариев 1.0. Если обязательный сценарий не проходит, версия остаётся pre-1.0 независимо от номера сборки.

Релиз 1.0 ориентирован на полную семантическую поддержку сцен, ресурсов и GDScript. Для C# в 1.0 обязательны обнаружение скриптов, связи со сценами, диагностика и доступные через language server символы. Полный паритет глубокого анализа C# выделяется в roadmap 1.x, если он потребует отдельного Roslyn-сервиса.

Нормативные ID требований `R1-01`–`R1-10`, точные pass criteria, матрица surface × platform, правила evidence и сквозной план release readiness определены в [RELEASE-001-release-1.0-acceptance-and-evidence-plan.md](RELEASE-001-release-1.0-acceptance-and-evidence-plan.md). Этот документ не меняет scope раздела 3, а задаёт способ доказать его выполнение.

## 4. Что не входит в обязательный scope 1.0

- гарантированно точный статический анализ динамических выражений вроде `load(variable)`, `get_node(variable)` и `call(method_name)`;
- управление удалённым редактором через публичную сеть;
- мобильная версия редактора;
- совместная многопользовательская редактура одной сцены;
- автоматическое выполнение всех write-операций без подтверждения;
- замена существующих GDScript/C# language servers;
- облачный индекс всех пользовательских проектов;
- поддержка каждой сторонней версии или форка Godot;
- обязательное включение интеграции в upstream Godot.

Динамические связи должны отображаться как `probable`, `dynamic` или `runtime-confirmed`, а не выдаваться за точные.

## 5. Архитектурный контур

Технические границы компонентов, lifecycle, revision/snapshot semantics, индекс, evidence model и исполнимый foundation backlog определены в [ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md](ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md).

```text
┌──────────────────────────────────────────────────────────┐
│                    Codex surfaces                        │
│  Codex App │ Codex CLI │ IDE │ Godot Codex Dock          │
└───────────────────────────┬──────────────────────────────┘
                            │ MCP / Codex app-server
┌───────────────────────────▼──────────────────────────────┐
│                  godot-codex-mcp sidecar                 │
│ MCP tools │ resources │ semantic index │ evidence model  │
└───────────────────────────┬──────────────────────────────┘
                            │ versioned local bridge RPC
┌───────────────────────────▼──────────────────────────────┐
│             modules/codex_bridge inside Godot            │
│ Editor state │ Resource graph │ GDScript │ Debugger       │
│ Transactions │ Undo/Redo │ Scene/runtime observation     │
└──────────────────────────────────────────────────────────┘
```

### 5.1. Godot Codex Bridge

Editor-only C++-модуль внутри форка Godot. Он является единственным компонентом, которому разрешено напрямую читать и изменять внутреннее состояние редактора.

Обязанности:

- получение текущей сцены и EditorSelection;
- чтение Inspector-свойств;
- доступ к EditorFileSystem, ResourceUID и зависимостям;
- использование GDScript parser/analyzer и доступного language-server контекста;
- доступ к EditorDebugger и runtime-объектам;
- выполнение изменений через EditorUndoRedoManager;
- публикация snapshots, revisions и событий;
- проверка project/session capability token;
- отсутствие прямой зависимости от OpenAI API и пользовательской авторизации.

### 5.2. Локальный Bridge RPC

Внутренний протокол между Godot и sidecar. Он не должен совпадать с MCP и должен оставаться небольшим, версионированным и независимым от UI Codex.

Основные требования:

- schema version и capability negotiation;
- request/response, notifications, cancellation;
- монотонный event sequence;
- `project_revision`, `scene_revision`, `runtime_session_id`;
- reconnect с запросом полного snapshot;
- backpressure и ограничение размера сообщений;
- Unix Domain Socket на macOS/Linux;
- Named Pipe или защищённый loopback fallback на Windows;
- session token, не записываемый в логи;
- discovery-файл внутри `.godot/codex/`, не попадающий в Git.

### 5.3. godot-codex-mcp

Отдельный локальный процесс, запускаемый Codex как stdio MCP server. Рекомендуемая production-реализация — отдельный single-binary sidecar; конкретный язык фиксируется архитектурным решением ADR-001.

Обязанности:

- обнаружение открытого Godot Editor для текущего проекта;
- handshake с bridge;
- предоставление MCP tools/resources;
- хранение нормализованного семантического индекса;
- маркировка источника и достоверности каждого результата;
- offline/read-only режим при закрытом редакторе;
- маршрутизация write approvals;
- санитарная обработка логов;
- совместимость с отдельным Codex, CLI, IDE и встроенным клиентом.

### 5.4. Семантический индекс

Индекс хранится вне основного потока редактора. Предпочтительное место — `.godot/codex/index/` или `.godot/codex/index.sqlite`, окончательное решение принимается после performance spike.

Сущности:

- Project;
- Resource и ResourceUID;
- Scene и SceneInheritance;
- Node и NodePath;
- Script;
- Symbol;
- Signal и SignalConnection;
- Group;
- ProjectSetting и Autoload;
- InputAction;
- RuntimeObject;
- Diagnostic;
- Evidence.

Основные связи:

- `contains`;
- `references`;
- `instantiates`;
- `inherits`;
- `attaches_script`;
- `connects_signal`;
- `belongs_to_group`;
- `preloads` / `loads`;
- `calls`;
- `overrides`;
- `runtime_instance_of`.

### 5.5. Evidence model

Каждый семантический ответ должен возвращать не только утверждение, но и основание:

```json
{
  "subject": "uid://example",
  "relation": "referenced_by",
  "path": "res://player/player.tscn",
  "node_path": "Player",
  "property": "stats",
  "line": null,
  "confidence": "exact",
  "freshness": "current",
  "source": "packed_scene_state",
  "project_revision": 1042
}
```

Допустимые уровни уверенности:

- `exact` — подтверждено структурой Godot или анализатором;
- `probable` — статический анализ с неоднозначностью;
- `dynamic` — значение вычисляется во время исполнения;
- `runtime-confirmed` — подтверждено активной runtime-сессией.

Свежесть хранится отдельно от уверенности: `current` означает факт на заявленной revision, `stale` — факт с предыдущей revision, требующий обновления. Это позволяет, например, не терять исходную метку `exact` у устаревшего результата и одновременно запрещает выдавать его за текущее состояние.

## 6. Предварительный каталог MCP-возможностей

### 6.1. Read tools

- `godot_project_overview`
- `godot_get_editor_state`
- `godot_get_current_scene`
- `godot_get_selected_nodes`
- `godot_inspect_node`
- `godot_get_open_scripts`
- `godot_get_resource_dependencies`
- `godot_find_usages`
- `godot_get_signal_connections`
- `godot_get_groups`
- `godot_get_project_settings`
- `godot_get_diagnostics`
- `godot_get_runtime_tree`
- `godot_inspect_runtime_object`
- `godot_get_stack_trace`
- `godot_capture_viewport`

### 6.2. Execution tools

- `godot_run_project`
- `godot_run_current_scene`
- `godot_stop_project`
- `godot_pause_project`
- `godot_continue_project`
- `godot_execute_test_scene`

### 6.3. Write tools

- `godot_create_node`
- `godot_delete_node`
- `godot_reparent_node`
- `godot_set_property`
- `godot_attach_script`
- `godot_connect_signal`
- `godot_disconnect_signal`
- `godot_create_resource`
- `godot_save_scene`
- `godot_apply_transaction`
- `godot_undo_transaction`

Имена и схемы являются предварительными. Финальный MCP contract фиксируется в MCP-001 и версионируется.

## 7. Общий Definition of Done для любого спринта

Задача не считается завершённой только потому, что код компилируется. Для каждого спринта обязательны:

- код реализует согласованный scope;
- новые публичные контракты документированы;
- добавлены unit/contract/integration tests по риску изменения;
- тесты проходят локально и в CI;
- нет необработанных секретов или персональных данных в логах;
- main thread Godot не выполняет неограниченную тяжёлую работу;
- ошибки возвращаются структурированно и пригодны для UI;
- version/capability mismatch имеет понятное поведение;
- документация спринта обновлена по фактической реализации;
- все acceptance criteria спринта продемонстрированы на fixture project;
- известные ограничения внесены в backlog, а не скрыты.

## 8. Метрики качества 1.0

Целевые значения уточняются после Sprint 3, но release gate должен включать:

- ноль ложных результатов с меткой `exact` на golden fixtures;
- не менее 95% найденных статически разрешимых resource/scene usages;
- cached read query p95 не более 300 мс на reference-medium проекте;
- получение текущей сцены p95 не более 500 мс;
- обновление изменённого файла в индексе p95 не более 2 секунд;
- отсутствие заметных editor frame stalls свыше согласованного бюджета;
- отсутствие повреждения сцен при fault-injection тестах;
- 100% write-операций имеют transaction ID и Undo path;
- отсутствие открытого сетевого listener за пределами loopback;
- успешный reconnect после перезапуска bridge или sidecar;
- успешная чистая установка на трёх целевых ОС.

---

## 9. План спринтов

## 9.1. Почему работы идут в таком порядке

Порядок построен вокруг раннего снятия архитектурных и продуктовых рисков:

1. **Сначала воспроизводимая база, затем форк-специфичный код.** Без стабильной сборки невозможно отличить регрессию интеграции от проблемы toolchain или upstream.
2. **Сначала тонкий end-to-end slice, затем широкий индекс.** Sprint 2 проверяет реальную цепочку Godot → bridge → sidecar → MCP → Codex до вложений в сложную модель данных.
3. **Статика строится снизу вверх: ресурсы → сцены → код.** ResourceUID и зависимости дают стабильные идентификаторы сценам; scene graph даёт контекст attached scripts; только после этого symbol references можно связать с игровыми сущностями.
4. **Evidence формализуется до расширения live/runtime контекста.** Иначе разные источники начнут возвращать несовместимые или неразличимые по достоверности факты.
5. **Read и diagnosis предшествуют write.** До изменения сцен система должна уметь точно читать текущее состояние, замечать stale revisions и диагностировать результат.
6. **Простые транзакции предшествуют составным.** Atomic multi-operation writes допустимы только после доказанного Undo и fault recovery на базовых операциях.
7. **Внешний клиент продуктизируется раньше встроенного.** Это отделяет качество семантического ядра и MCP от сложности UI; встроенная панель затем повторно использует уже проверенный контракт.
8. **Hardening и три ОС предшествуют scope freeze.** Beta feedback должен относиться к продукту, близкому к реальному способу установки и эксплуатации.
9. **Реальные проекты предшествуют RC.** API и схемы замораживаются только после проверки accuracy, latency и UX вне синтетических fixtures.

Каждый следующий этап должен опираться на доказательства предыдущего gate. Календарное окончание спринта не разрешает переход, если acceptance criteria не выполнены.

### Sprint 0 — Bootstrap и воспроизводимая база

**Длительность:** 1 неделя
**Milestone:** Build Baseline

#### Цель

Получить управляемый форк, который стабильно собирается и может безопасно принимать долгоживущие изменения.

#### Работы

- Зафиксировать выбранный base commit и стратегию обновления upstream.
- Добавить официальный `godotengine/godot` как upstream remote.
- Решить, нужна ли полная история или ограниченный fetch для сопровождения форка.
- Создать integration branch с префиксом `codex/`.
- Установить и зафиксировать версии Python, SCons и platform toolchain.
- Собрать editor/dev build на macOS.
- Создать минимальный fixture project.
- Настроить CI для сборки editor target и базовых тестов.
- Добавить каталог проектной документации и ADR index.
- Зафиксировать versioning policy движка, bridge, sidecar и protocol.

#### Артефакты

- `BUILDING_CODEX_FORK.md`;
- ADR-000: fork/upstream strategy;
- CI workflow;
- fixture `tests/codex/fixtures/smoke_project`;
- воспроизводимая команда сборки.

#### Критерии приёмки

- чистый checkout собирается по документации;
- собранный editor открывает fixture project;
- базовый Godot test suite не получает новых падений;
- CI повторяет локальную сборку;
- зафиксирован владелец процесса обновления upstream.

#### Gate

Никакая разработка bridge не начинается, пока baseline build не воспроизводится.

### Sprint 1 — Архитектурный каркас и protocol skeleton

**Длительность:** 2 недели
**Milestone:** Internal Bridge Skeleton

#### Цель

Создать editor-only модуль и минимальный безопасный локальный канал без MCP и OpenAI-зависимостей.

#### Работы

- Создать `modules/codex_bridge` и build options.
- Ограничить регистрацию модуля editor build.
- Определить lifecycle bridge: start, project loaded, shutdown.
- Спроектировать discovery-файл и session token.
- Реализовать локальный transport для macOS.
- Добавить `initialize`, `ping`, `capabilities`, `shutdown`.
- Добавить schema version и structured errors.
- Реализовать request cancellation и timeout foundation.
- Добавить redacted logging и отдельную debug category.
- Подготовить protocol schema generation/validation.

#### Артефакты

- ADR-001: component boundaries and language choice;
- PROTOCOL-001: Bridge RPC v1;
- C++ module skeleton;
- protocol conformance test client.

#### Критерии приёмки

- test client обнаруживает открытый editor;
- handshake отклоняет неправильный token;
- mismatch protocol version возвращает структурированную ошибку;
- editor закрывается без зависшего socket/process;
- bridge отключён в export templates и non-editor build.

### Sprint 2 — Первый end-to-end MCP vertical slice

**Длительность:** 2 недели
**Milestone:** Architecture Proof / M0

#### Цель

Доказать, что отдельный Codex получает живой контекст из открытого Godot.

#### Работы

- Создать skeleton `godot-codex-mcp`.
- Реализовать stdio MCP lifecycle.
- Реализовать обнаружение bridge по project root.
- Добавить project-scoped Codex configuration template.
- Добавить read tools для editor state, current scene и selection.
- Передавать тип узла, NodePath, owner, script и выбранные свойства.
- Добавить `scene_revision` и признак unsaved state.
- Создать минимальный Codex usage guide.
- Реализовать integration test без участия модели.
- Провести ручной тест из отдельного приложения Codex.

#### Артефакты

- MCP-001 draft;
- sidecar executable;
- `.codex/config.toml.example`;
- fixture-сценарий live selection;
- demo recording/checklist.

#### Критерии приёмки

1. Пользователь открывает fixture project в Godot.
2. Выбирает `CharacterBody2D`.
3. Меняет экспортированное свойство, не сохраняя сцену.
4. В отдельном Codex спрашивает о выбранном узле.
5. Codex через MCP получает точный NodePath, script и несохранённое значение.

Если этот сценарий нестабилен, разработка индекса блокируется до устранения архитектурной причины.

### Sprint 3 — ResourceUID и граф файловых зависимостей

**Длительность:** 2 недели
**Milestone:** Static Index Foundation
**Детальный план:** [SPRINT-3-PLAN.md](SPRINT-3-PLAN.md)

#### Цель

Построить инкрементальный индекс ресурсов и прямых/обратных зависимостей.

#### Работы

- Определить schema semantic index.
- Индексировать ResourceUID, path, type, import status и mtime/hash.
- Получать зависимости через Godot ResourceLoader/EditorFileSystem.
- Строить reverse dependency edges.
- Обрабатывать `.tscn`, `.scn`, `.tres`, `.res` и imported resources.
- Обрабатывать перемещение, переименование, удаление и reimport.
- Добавить incremental update queue.
- Вынести тяжёлую запись индекса из main thread.
- Реализовать invalidation и full rebuild.
- Добавить index format migration/version.

#### Артефакты

- INDEX-001: entity/edge/storage model;
- индексатор;
- `godot_get_resource_dependencies`;
- `godot_find_resource_owners`;
- golden dependency fixtures.

#### Критерии приёмки

- прямые и обратные зависимости совпадают с golden graph;
- переименование ресурса обновляет индекс без полного rebuild;
- stale UID явно диагностируется;
- повторное открытие проекта использует совместимый cache;
- rebuild можно отменить без повреждения индекса.

### Sprint 4 — Семантика сцен и node graph

**Длительность:** 2 недели
**Milestone:** Scene Understanding

#### Цель

Представить сцены как структурированный граф, а не текстовый файл.

#### Работы

- Индексировать PackedScene state.
- Обрабатывать scene inheritance и instanced scenes.
- Индексировать NodePath, owner, editable children и internal nodes.
- Индексировать attached scripts и exported properties.
- Индексировать external/subresources.
- Индексировать signal connections и groups.
- Индексировать Animation track NodePath.
- Индексировать Autoload, InputMap и релевантные ProjectSettings.
- Различать local override и inherited value.
- Добавить canonical identifiers для scene/node entities.

#### Артефакты

- SCENE-001: scene semantic model;
- scene graph indexer;
- `godot_get_scene_graph`;
- `godot_inspect_node`;
- fixtures наследования, инстансов, сигналов и анимаций.

#### Критерии приёмки

- ответ показывает источник inherited/overridden property;
- instanced scene прослеживается до исходной сцены;
- signal connection содержит emitter, signal и receiver;
- Animation NodePath разрешается либо маркируется broken;
- результат не зависит от порядка секций в текстовом `.tscn`.

### Sprint 5 — GDScript symbols и программные связи

**Длительность:** 2 недели
**Milestone:** Script Semantics

Исполнимая декомпозиция `S5-01`–`S5-10`, решение `D-07`, версии
Bridge/index, MCP-контракт и локальная macOS/Windows приёмка определены в
[SPRINT-5-PLAN.md](SPRINT-5-PLAN.md).

`S5-01`–`S5-03` закрыты локально на macOS arm64: канонический
[SCRIPT-001](SCRIPT-001-gdscript-and-csharp-adapters.md) выбирает bridge-owned
saved-content cache поверх Godot parser/analyzer, а committed contract evidence
фиксирует source scope, vectors, D-07 spike и GDScript-disabled build. Отдельный
production-independent fixture/golden oracle фиксирует documents, declarations,
relations, diagnostics, ranges и attachment states до wire/index реализации.
Bridge RPC 1.4 фиксирует strict script schemas, negotiation, downgrade omission
и resource/scene compatibility в C++/Rust. Следующий gate — production
`ScriptSemanticAdapter` и bounded journal `S5-04`; полная Sprint 5 приёмка пока
не заявляется.

#### Цель

Добавить GDScript-символы и связи между кодом, сценами и ресурсами.

#### Работы

- Интегрироваться с GDScript parser/analyzer или существующим language-server слоем.
- Индексировать classes, methods, properties, constants и signals.
- Индексировать inheritance и overrides.
- Индексировать preload/load с литеральными путями и UID.
- Связывать attached script с scene nodes.
- Индексировать точные symbol references, где анализатор их разрешает.
- Маркировать динамические вызовы и строковые NodePath.
- Добавить file/line/range evidence.
- Обрабатывать parse errors без остановки индексации проекта.
- Добавить минимальную C# discovery/diagnostics abstraction.

#### Артефакты

- SCRIPT-001: language semantic adapter;
- GDScript symbol index;
- symbol query tools;
- fixtures typed/untyped/dynamic GDScript.

#### Критерии приёмки

- точные references имеют корректные line/range;
- override связан с base method;
- parse error возвращается как diagnostic и не ломает индекс;
- динамический вызов не получает метку `exact`;
- смена script attachment обновляет связи сцены.

### Sprint 6 — Find usages, evidence и контекст для Codex

**Длительность:** 2 недели
**Milestone:** Semantic Alpha / M1

#### Цель

Собрать данные предыдущих спринтов в надёжные запросы, пригодные для рассуждений Codex.

#### Работы

- Реализовать единый `godot_find_usages`.
- Добавить фильтры entity kind, confidence и scope.
- Реализовать evidence aggregation и deduplication.
- Добавить project/scene summaries с ограниченным token budget.
- Реализовать pagination и result limits.
- Добавить MCP resources для стабильного read-only контекста.
- Создать Godot-specific AGENTS.md/skill guidance template.
- Определить правила, когда Codex обязан сначала запросить semantic tools.
- Добавить explainability output для UI и logs.
- Создать semantic accuracy benchmark.

#### Артефакты

- CONTEXT-001: model-facing context contract;
- EVIDENCE-001;
- read-only MCP toolset alpha;
- benchmark report;
- semantic fixture suite.

#### Критерии приёмки

- zero false `exact` на golden fixtures;
- каждый usage имеет evidence;
- summaries укладываются в заданный output budget;
- одинаковые факты не дублируются из разных индексаторов;
- внешний Codex отвечает на контрольные вопросы со ссылкой на semantic evidence.

### Sprint 7 — Полный live editor context

**Длительность:** 2 недели
**Milestone:** Live Editor Alpha

#### Цель

Сделать живой редактор авторитетным источником для открытых и несохранённых сцен.

#### Работы

- Поддержать все открытые scene tabs.
- Получать EditorSelection и multi-selection.
- Получать текущий Inspector object и editable properties.
- Получать открытые scripts, active script и selection range.
- Различать disk state и editor state.
- Реализовать scene revisions и dirty markers.
- Публиковать summary native Undo/Redo histories: history ID, action name, saved/unsaved state, наличие Undo/Redo и монотонный operation sequence.
- Добавить editor diagnostics/output snapshot.
- Добавить viewport metadata и optional screenshot capture foundation.
- Обрабатывать закрытие сцены, смену проекта и reimport.
- Не сериализовать опасные/огромные Variant значения без лимитов.

#### Артефакты

- EDITOR-001: live context contract;
- editor resources/tools;
- unsaved-state и native-history fixtures;
- revision conflict tests.

#### Критерии приёмки

- несохранённое свойство имеет приоритет над disk state;
- ответ явно сообщает, что сцена dirty;
- multi-selection не теряет идентичность узлов;
- stale scene revision обнаруживается;
- native editor action, Undo и Redo отражаются в history summary и обновляют соответствующие revisions;
- неизвестный payload сторонней editor operation остаётся opaque и не получает выдуманных деталей;
- сериализация циклических/больших объектов ограничена и безопасна.

### Sprint 8 — Runtime и EditorDebugger

**Длительность:** 2 недели
**Milestone:** Diagnostic MVP / M2

#### Цель

Дать Codex наблюдаемость запущенной игры и возможность связывать runtime с editor entities.

#### Работы

- Интегрироваться с EditorDebugger lifecycle.
- Реализовать run current scene/project, stop, pause и continue.
- Получать remote scene tree.
- Получать runtime object properties с лимитами.
- Получать errors, warnings и stack traces.
- Ввести `runtime_session_id`.
- Сопоставлять runtime nodes с source scene/node, где возможно.
- Добавить viewport screenshot tool.
- Добавить timeout/crash/disconnect semantics.
- Реализовать read-only diagnostic workflow.

#### Артефакты

- RUNTIME-001;
- runtime MCP tools;
- diagnostic fixture project;
- end-to-end crash/error scenarios.

#### Критерии приёмки

- Codex запускает fixture и получает runtime tree;
- deliberate error возвращает корректный stack trace;
- перезапуск игры создаёт новый runtime session;
- editor node и runtime instance сопоставляются с evidence;
- зависший или упавший game process не зависает bridge.

### Sprint 9 — Безопасные editor transactions

**Длительность:** 2 недели
**Milestone:** Write Foundation

#### Цель

Разрешить первые изменения сцены исключительно через транзакции Godot Editor.

#### Работы

- Спроектировать transaction lifecycle: plan, preview, approve, apply, validate, undo.
- Интегрировать EditorUndoRedoManager.
- Добавить optimistic concurrency через scene revision.
- Реализовать create/delete/reparent node.
- Реализовать set property.
- Реализовать attach/detach script.
- Реализовать connect/disconnect signal.
- Добавить dry-run preview.
- Добавить transaction journal без секретов и полных чувствительных значений.
- Запретить raw modification открытой `.tscn` через bridge tools.

#### Артефакты

- WRITE-001: transaction and approval model;
- базовые write tools;
- preview schema;
- undo/fault-injection test suite.

#### Критерии приёмки

- каждая операция создаёт transaction ID;
- stale revision отклоняется до изменения;
- Undo полностью восстанавливает сцену;
- ошибка в середине операции не оставляет half-applied state;
- destructive tools требуют соответствующего approval hint.

### Sprint 10 — Составные изменения и автоматическая валидация

**Длительность:** 2 недели
**Milestone:** Read/Write Beta Foundation / M3

#### Цель

Позволить Codex выполнять содержательные многошаговые изменения и доказывать их корректность.

#### Работы

- Реализовать atomic multi-operation transactions.
- Добавить create/update resource.
- Добавить scene save с явным scope.
- Добавить script reload/reparse после изменений.
- Добавить automatic diagnostics check.
- Добавить optional run validation после apply.
- Сравнивать pre/post semantic graph.
- Возвращать structured validation report.
- Добавить rollback policy при validation failure.
- Добавить user-selectable confirmation policy.

#### Артефакты

- advanced write toolset;
- VALIDATION-001;
- transaction scenario fixtures;
- read/write beta checklist.

#### Критерии приёмки

- составная операция либо применяется полностью, либо полностью откатывается;
- после изменения обновляются индекс и editor state;
- validation report содержит diagnostics и affected entities;
- Codex может добавить узел, подключить сигнал, изменить скрипт и проверить сцену;
- пользователь может отменить результат стандартным Undo.

### Sprint 11 — Продуктизация внешнего Codex

**Длительность:** 2 недели
**Milestone:** External Codex Beta

#### Цель

Сделать внешний Codex основным пригодным для ежедневной работы клиентом интеграции.

#### Работы

- Финализировать MCP tool schemas и descriptions.
- Добавить setup/doctor command.
- Генерировать или устанавливать project-scoped config только с согласия пользователя.
- Добавить skill/AGENTS guidance package.
- Добавить connection status и понятные remediation messages.
- Поддержать несколько открытых Godot-проектов.
- Добавить offline/cache mode при закрытом editor.
- Добавить compatibility matrix Codex/Godot/bridge.
- Документировать approvals и sandbox expectations.
- Провести task-based usability test.

#### Артефакты

- внешний beta installer/package;
- setup and troubleshooting guide;
- `godot-codex doctor`;
- final MCP-001 v1;
- external Codex acceptance report.

#### Критерии приёмки

- новый пользователь подключает проект по документации;
- Codex видит только соответствующий project instance;
- закрытый editor даёт понятный offline status;
- doctor различает missing binary, stale discovery, version mismatch и auth/config issue;
- основной набор read/write/runtime сценариев проходит в Codex App, Codex CLI и поддерживаемой IDE-интеграции;
- entity identifiers, evidence, revisions и approval semantics совпадают между внешними поверхностями.

### Sprint 12 — Встроенный Codex client shell

**Длительность:** 2 недели
**Milestone:** Embedded Client Alpha

#### Цель

Встроить в Godot минимальный полноценный клиент Codex на базе Codex app-server.

#### Работы

- Создать Codex Dock и lifecycle UI.
- Запускать/обнаруживать `codex app-server` безопасным способом.
- Реализовать initialize/initialized handshake.
- Реализовать account state и поддерживаемый login flow.
- Реализовать thread start/resume/list.
- Реализовать turn start/steer/interrupt.
- Отображать streamed agent message deltas.
- Отображать command/tool/file-change items.
- Обрабатывать app-server crash/restart/version mismatch.
- Использовать тот же Godot MCP, что и внешний Codex.

#### Артефакты

- UI-001: embedded client architecture;
- app-server protocol adapter;
- базовый Codex Dock;
- auth/thread lifecycle tests.

#### Критерии приёмки

- пользователь входит поддерживаемым способом;
- создаёт и продолжает thread;
- получает потоковый ответ;
- может interrupt активный turn;
- app-server restart не повреждает Godot project и даёт recoverable UI state.

### Sprint 13 — Встроенный UX, approvals и diff

**Длительность:** 2 недели
**Milestone:** Embedded Beta / M4

#### Цель

Довести встроенную панель до функционального паритета с основным ежедневным workflow.

#### Работы

- Добавить task/thread navigation.
- Добавить context chips: project, scene, node, script, diagnostic.
- Добавить plan/progress presentation.
- Добавить approval UI для команд и Godot write transactions.
- Добавить file diff и semantic scene preview.
- Добавить apply/undo controls.
- Добавить model/reasoning settings из доступных app-server capabilities.
- Добавить history, archive и resume.
- Добавить accessibility, keyboard navigation и theme integration.
- Добавить error/reconnect/empty states.

#### Артефакты

- UX specification;
- production-like Codex Dock;
- approval/diff components;
- embedded usability report.

#### Критерии приёмки

- пользователь завершает diagnostic и write сценарии, не покидая Godot;
- ни одна destructive operation не скрыта в обычном текстовом сообщении;
- preview показывает scene operations отдельно от file diff;
- UI остаётся отзывчивым во время длинного turn;
- thread можно продолжить после перезапуска редактора.

### Sprint 14 — Performance, надёжность и безопасность

**Длительность:** 2 недели
**Milestone:** Production Hardening

#### Цель

Устранить архитектурные риски перед кроссплатформенным выпуском.

#### Работы

- Создать small/medium/large benchmark projects.
- Профилировать initial indexing и incremental updates.
- Ограничить main-thread work budget.
- Добавить request/result size limits и pagination.
- Добавить queue backpressure и cancellation.
- Провести threat model локального bridge.
- Проверить token/discovery permissions.
- Добавить fuzz/invalid-message tests.
- Добавить process crash, editor crash и stale cache recovery.
- Добавить privacy/log redaction audit.
- Проверить sandbox и approval defaults.
- Добавить protocol compatibility tests N/N-1, если поддерживается.

#### Артефакты

- SECURITY-001 threat model;
- PERFORMANCE-001;
- reliability test matrix;
- benchmark dashboard/report;
- hardening backlog с severity.

#### Критерии приёмки

- release performance budgets выполнены либо формально пересмотрены;
- нет listener на non-loopback интерфейсе;
- случайный локальный процесс без token не получает editor state;
- corrupt index автоматически перестраивается;
- sidecar crash не приводит к crash редактора;
- high-severity security findings закрыты.

### Sprint 15 — Windows, Linux и packaging

**Длительность:** 2 недели
**Milestone:** Cross-platform Feature Complete

#### Цель

Обеспечить одинаковый пользовательский workflow на трёх desktop ОС.

#### Работы

- Реализовать/проверить Windows transport и process lifecycle.
- Проверить Linux Unix socket, permissions и desktop environments.
- Настроить CI matrix macOS/Windows/Linux.
- Собрать signed/notarized artifacts, где применимо.
- Определить правила обнаружения sidecar/app-server binaries.
- Создать installer/uninstaller/update path.
- Проверить пути с пробелами, Unicode и длинные Windows paths.
- Проверить antivirus/firewall false positives.
- Проверить project migration между ОС.
- Документировать platform-specific troubleshooting.

#### Артефакты

- PLATFORM-001;
- CI matrix;
- установочные пакеты;
- platform acceptance reports;
- release artifact manifest.

#### Критерии приёмки

- чистая установка проходит на каждой целевой ОС;
- одинаковые semantic fixtures дают эквивалентные результаты;
- reconnect и process shutdown работают на каждой ОС;
- uninstall не удаляет пользовательские проекты или Codex history;
- package provenance/checksums опубликованы.

### Sprint 16 — Beta validation и реальная эксплуатация

**Длительность:** 2 недели
**Milestone:** Public/Closed Beta / M5

#### Цель

Проверить продукт на реальных проектах и задачах перед заморозкой API 1.0.

#### Работы

- Подготовить beta/RC distribution channel.
- Выбрать проекты разного размера и жанра.
- Провести сценарии comprehension, find usages, diagnosis и write changes.
- Собирать opt-in технические метрики без содержимого проектов.
- Классифицировать failures: accuracy, latency, UX, compatibility, crash.
- Провести API/tool schema review по реальным model traces.
- Исправить blocker/critical/high issues.
- Провести documentation usability test.
- Зафиксировать 1.0 scope freeze.
- Зафиксировать scope-freeze commit и создать `codex/release-1.0`.
- Сформировать release notes draft и known limitations.

#### Артефакты

- beta builds;
- beta test protocol;
- anonymized issue taxonomy;
- scope freeze document;
- release branch от зафиксированного scope-freeze commit;
- release candidate backlog.

#### Критерии приёмки

- все release acceptance scenarios проходят минимум на трёх реальных проектах;
- нет открытых blocker/critical issues;
- high issues имеют fix или формально принятое ограничение;
- MCP/bridge schemas заморожены для 1.0;
- установка понятна пользователю без помощи разработчика.

### Sprint 17 — Release Candidate и стабильный релиз 1.0

**Длительность:** 2 недели
**Milestone:** RC → Stable 1.0 / M6

#### Цель

Провести полный release audit, выпустить RC, устранить регрессии и опубликовать 1.0.

#### Работы

- Выполнить RC version bump в подготовленной release branch.
- Запустить полный unit/contract/integration/E2E suite.
- Запустить performance и security regression suites.
- Провести чистую установку и upgrade test на трёх ОС.
- Проверить compatibility с заявленными версиями Codex.
- Проверить license notices и redistribution policy.
- Проверить crash recovery и data migration.
- Провести manual acceptance checklist.
- Выпустить RC и выдержать stabilization window.
- Исправить release-blocking regressions.
- Подписать и опубликовать stable artifacts.
- Опубликовать документацию, release notes, known limitations и support policy.
- Создать roadmap 1.1.

#### Артефакты

- RELEASE-001;
- RC и stable artifacts;
- SBOM/license notices, если применимо;
- checksums/signatures;
- migration and rollback guide;
- support and compatibility policy;
- roadmap 1.1.

#### Критерии приёмки

- все критерии раздела 3 доказаны тестами или manual acceptance evidence;
- все метрики раздела 8 подтверждены отчётом;
- нет blocker/critical/high defects;
- нет известных сценариев повреждения проекта;
- release artifacts воспроизводимы и проверяемы;
- документация соответствует фактическому продукту;
- stable 1.0 устанавливается и проходит smoke workflow на каждой целевой ОС.

---

## 10. Milestone map

| Milestone | После спринта | Что доказано |
|---|---:|---|
| Build Baseline | 0 | Форк воспроизводимо собирается |
| M0 Architecture Proof | 2 | Внешний Codex видит живой выбранный узел |
| M1 Semantic Alpha | 6 | Работают индекс, find usages и evidence |
| M2 Diagnostic MVP | 8 | Codex понимает editor + runtime |
| M3 Read/Write Foundation | 10 | Безопасные составные изменения с Undo |
| External Codex Beta | 11 | Внешний клиент пригоден для ежедневной работы |
| M4 Embedded Beta | 13 | Полноценная панель внутри Godot |
| Production Hardening | 14 | Выполнены security/performance/reliability gates |
| Cross-platform Feature Complete / Beta Gate | 15 | macOS, Windows и Linux готовы к реальной Beta |
| M5 Beta Validated / RC Readiness | 16 | Реальная эксплуатация пройдена, scope и контракты 1.0 заморожены |
| M6 RC → Stable 1.0 | 17 | RC стабилизирован и полный release audit пройден |

## 11. Release gates

Release gates являются накопительными: более поздняя стадия обязана сохранять все доказательства предыдущих стадий. Gate оценивается по артефактам и результатам тестов, а не по календарной дате или проценту закрытых задач. Невыполненный критерий возвращает работу в спринт-владелец; он не переносится молча в known limitations.

Для release triage используются следующие уровни:

- **blocker** — продукт нельзя установить, запустить или использовать по основному сценарию; возможна потеря/повреждение проекта;
- **critical** — нарушение trust boundary, утечка данных/секретов, воспроизводимый crash или неверное изменение без надёжного recovery;
- **high** — обязательный acceptance scenario не работает, выдаётся ложное `exact`, нарушен заявленный performance/compatibility budget либо отсутствует безопасный workaround;
- **medium/low** — локальная проблема с документированным workaround, не нарушающая обязательные сценарии и инварианты безопасности.

### 11.1. Alpha gate — после Sprint 8

Alpha — ограниченно распространяемая сборка для проверки понимания проекта и диагностики. Она ещё не обязана предоставлять write-функции, встроенный клиент и кроссплатформенный installer.

Сборка считается Alpha, если:

- пройдены M0, M1 и M2: live selection, semantic index, evidence, editor state и runtime diagnosis работают end-to-end;
- внешний Codex выполняет сценарии A–D раздела 13 на основной платформе macOS;
- fixture suite подтверждает ноль ложных `exact` и корректную маркировку stale/dynamic данных;
- bridge и sidecar проходят token, reconnect, cancellation и crash-isolation tests;
- отсутствует listener за пределами разрешённого локального transport;
- есть versioned alpha package, setup guide, compatibility range и список известных ограничений;
- нет открытых blocker/critical defects.

Допустимо в Alpha: несовместимые изменения схем с migration note, ручная установка, ограниченная C#-семантика, отсутствие write tools и embedded UI.

### 11.2. Beta gate — после Sprint 15

Beta — feature-complete реализация scope 1.0, предназначенная для реальных проектов на всех целевых ОС. После этого gate новые обязательные функции в 1.0 не добавляются без формального изменения roadmap.

Сборка считается Beta, если:

- Codex App, CLI, поддерживаемая IDE-интеграция и встроенный клиент выполняют один и тот же read/runtime/write workflow через общий MCP;
- транзакции plan → preview → approve → apply → validate → undo доказаны fault-injection и E2E tests;
- сценарии A–J раздела 13 проходят на reference fixtures на macOS, Windows и Linux;
- security, privacy, recovery и performance criteria Sprint 14 выполнены;
- clean install, update, doctor, reconnect и uninstall проходят на трёх ОС;
- MCP/Bridge schemas имеют объявленную beta version и migration policy;
- документация достаточна для самостоятельного подключения beta-тестера;
- нет blocker/critical defects; high-дефекты перечислены, имеют владельца и план закрытия до RC.

Допустимо в Beta: исправления API по результатам реальной эксплуатации, medium/low defects и явно документированные ограничения, не отменяющие определение 1.0.

### 11.3. Release Candidate readiness gate — после Sprint 16

RC — кандидат с замороженными scope, MCP contract, Bridge RPC и форматом индекса. Сборка получает статус RC после прохождения этого gate и публикации идентифицированных RC artifacts в начале Sprint 17. После публикации RC допустимы только исправления дефектов, документации, packaging и release engineering; любое несовместимое изменение повторно открывает Beta gate.

Сборка готова к публикации как RC, если:

- все сценарии A–J проходят на трёх целевых ОС и минимум на трёх репрезентативных реальных проектах;
- все требования раздела 3 сопоставлены с test run, отчётом или подписанным manual acceptance evidence;
- достигнуты метрики раздела 8 на зафиксированных benchmark projects;
- scope/API freeze зафиксирован версиями схем и release branch;
- выполнены threat-model review, dependency/license review и проверка redaction;
- clean install и upgrade с последней Beta проходят без потери настроек, индекса, сцен или Codex history;
- нет blocker/critical/high security или data-integrity defects;
- остальные high-дефекты закрыты либо имеют письменное release exception с владельцем, обоснованием, workaround и сроком; исключение не может отменять обязательный сценарий 1.0;
- подготовлены RC artifacts, checksums, release notes draft, known limitations и rollback guide;
- release branch создана от зафиксированного scope-freeze commit, а RC build воспроизводимо собирается из неё.

### 11.4. Stable 1.0 gate — после Sprint 17

Stable 1.0 — поддерживаемая сборка, полностью соответствующая определению раздела 3.

Релиз считается Stable, если:

- RC выдержал не менее семи календарных дней и один полный regression cycle без нового release blocker;
- повторно пройдены unit, contract, integration, E2E, security, performance, install, upgrade и recovery suites на финальных подписанных artifacts;
- все сценарии A–J повторены на финальном build, а evidence package архивирован вместе с release manifest;
- нет blocker/critical/high defects и нет известных сценариев повреждения пользовательского проекта;
- опубликованы проверяемые artifacts, checksums/signatures, notices, SBOM при применимости и provenance;
- документация, compatibility matrix, support policy, migration/rollback guide и known limitations соответствуют финальному поведению;
- назначены владельцы поддержки 1.0 и принят roadmap 1.1;
- release owner, engineering owner, QA и security reviewer подписали release checklist.

Стабильность не означает отсутствие medium/low defects. Она означает, что они перечислены, имеют безопасный workaround или не затрагивают обязательные сценарии, безопасность, целостность данных и заявленную совместимость.

### 11.5. Обязательный evidence package для gate review

Для каждого Alpha/Beta/RC/Stable решения сохраняются:

- идентификаторы точных commits и версий bridge, sidecar, Codex и protocol schemas;
- OS/toolchain matrix и hashes проверенных artifacts;
- ссылки на CI runs, benchmark/security reports и fixture versions;
- результаты применимых сценариев A–J с pass/fail и ссылкой на доказательство;
- список открытых defects по severity;
- принятые release exceptions;
- имена ответственных и дата решения go/no-go.

## 12. Зависимости и допустимый параллелизм

Последовательность критического пути:

```text
S0 → S1 → S2 → S3 → S4 → S5 → S6 → S7 ─┬→ S8 ─┐
                                         └→ S9 ─┴→ S10 → S11 → S12 → S13
                                                                      ↓
                                            S17 ← S16 ← S15 ← S14 ←───┘
```

S8 и S9 могут идти параллельно после S7, но S10 принимает результаты обоих: runtime необходим для post-change validation, а transaction foundation — для безопасного apply/rollback.

| Спринт | Жёсткие зависимости | Что зависимость гарантирует |
|---|---|---|
| S0 | нет | Воспроизводимая база сборки и тестов |
| S1 | S0 | Bridge разрабатывается на контролируемом форке и toolchain |
| S2 | S1 | MCP vertical slice использует версионированный и защищённый transport |
| S3 | S2 | Индекс строится после доказанной end-to-end архитектуры |
| S4 | S3 | Сцены используют стабильные ResourceUID и dependency edges |
| S5 | S3, зафиксированная entity model S4 | Скриптовые символы связываются с каноническими scene/resource entities |
| S6 | S3–S5 | Find usages агрегирует уже нормализованные ресурсы, сцены и код |
| S7 | S6 | Live editor overlay совместим с evidence model и disk index |
| S8 | S7 | Runtime entities сопоставляются с editor entities и revisions |
| S9 | S6, S7 | Write preconditions опираются на точное live state и stale detection |
| S10 | S8, S9 | Составные изменения можно валидировать runtime/diagnostics и откатывать |
| S11 | S6, S10 | Внешний клиент продуктизирует полный read/runtime/write contract |
| S12 | S11 для приёмки; mock после S6 | Embedded client переиспользует проверенный MCP и app-server lifecycle |
| S13 | S10, S12 | Approval/diff UX управляет реальными транзакциями, а не mock-операциями |
| S14 | S10, S11, S13 | Hardening охватывает весь feature-complete workflow |
| S15 | S14 | Packaging переносит уже защищённый и профилированный продукт |
| S16 | S11, S13–S15 | Реальная Beta проверяет обе поверхности и три ОС |
| S17 | S16 и RC readiness gate | Stable выпускается только после scope freeze и реальной эксплуатации |

При наличии команды часть работ может идти параллельно:

- schema/fixture work для S4 может начинаться во время S3;
- GDScript adapter S5 может разрабатываться параллельно scene index S4 после фиксации entity model;
- embedded UI S12 может начинаться после S6 с mock app-server, но интеграция принимается только после S11;
- Windows/Linux transport prototypes можно начать после S2;
- security threat modeling начинается в S1 и формально закрывается в S14;
- documentation и installer work ведутся инкрементально, а не откладываются до S17.

Оценка календаря:

- один разработчик: около 35 недель до 1.0 без больших архитектурных переделок;
- два разработчика: ориентировочно 24–28 недель;
- три специализированных потока (engine/index, sidecar/protocol, UI/release): ориентировочно 20–24 недели.

Оценки не включают длительные внешние блокировки, обязательную upstream-приёмку или глубокий Roslyn-паритет.

## 13. Release acceptance scenarios

Перед 1.0 каждый сценарий выполняется на macOS, Windows и Linux, если он не является platform-specific.

Детальные предусловия, pass criteria, обязательные test layers и evidence для этих сценариев определены в [RELEASE-001-release-1.0-acceptance-and-evidence-plan.md](RELEASE-001-release-1.0-acceptance-and-evidence-plan.md). Сценарии A–J являются читаемыми release workflows; нормативные требования `R1-01`–`R1-10` используются для трассировки и автоматической проверки полноты gate.

### A. Live selection

- открыть сцену;
- выбрать узел;
- изменить свойство без сохранения;
- запросить состояние через внешний Codex;
- получить точное несохранённое значение и scene revision.

### B. Find usages

- выбрать ресурс по UID;
- найти все точные владельцы;
- отдельно показать probable/dynamic usages;
- перейти к сцене, узлу, property или line evidence.

### C. Scene comprehension

- объяснить наследование и instanced scenes;
- показать local overrides;
- показать signals, groups и animation NodePaths;
- не смешать source scene и runtime instance.

### D. Runtime diagnosis

- запустить проект;
- получить runtime tree;
- воспроизвести deliberate error;
- получить stack trace и связанный source symbol;
- предложить исправление с evidence.

### E. Safe modification

- получить исходное состояние editor history и revisions;
- добавить узел;
- изменить property;
- подключить signal;
- применить script change;
- показать preview;
- подтвердить;
- выполнить validation run;
- убедиться, что transaction отражена в editor history и Undo доступен;
- отменить всю операцию через Undo;
- подтвердить обновление history/revisions и восстановление исходного semantic graph.

### F. Recovery

- оборвать sidecar во время read request;
- восстановить соединение;
- оборвать sidecar во время подготовленной, но не применённой транзакции;
- убедиться, что сцена не изменилась;
- перестроить повреждённый индекс.

### G. Multi-project isolation

- открыть два разных Godot-проекта;
- подключить два Codex workspace;
- убедиться, что контекст, токены и операции не пересекаются.

### H. Embedded client

- войти в Codex;
- создать thread;
- выполнить diagnostic task;
- подтвердить write transaction;
- просмотреть diff/semantic preview;
- перезапустить Godot и продолжить thread.

### I. Client surface parity

- подключить один fixture project к Codex App, Codex CLI и поддерживаемой IDE-интеграции через один project-scoped MCP contract;
- выполнить project overview, live selection, find usages и runtime diagnosis;
- подготовить write transaction, проверить одинаковые preview/approval semantics и отменить изменение;
- сравнить entity identifiers, evidence, revisions и structured errors между клиентами;
- убедиться, что ни одна внешняя поверхность не использует отдельный семантический индекс или client-specific Godot bridge.

### J. Cross-platform installation lifecycle

- на чистых образах macOS, Windows и Linux проверить provenance и checksum/signature release artifacts;
- установить специальную сборку Godot, bridge/sidecar и project-scoped Codex integration по опубликованной инструкции;
- запустить doctor и выполнить smoke workflow live selection, runtime, safe write/Undo и reconnect;
- обновиться с заявленной предыдущей версии и повторить doctor/smoke workflow;
- удалить интеграцию и убедиться, что пользовательский проект и сохраняемая по policy Codex history не удалены;
- проверить отсутствие orphan processes/listeners и сохранить platform evidence package.

## 14. Основные риски

| Риск | Последствие | Снижение риска |
|---|---|---|
| Разработка на moving Godot master | Постоянные конфликты и поломки | Pin base commit, регулярный контролируемый upstream sync |
| Использование внутренних editor API | Высокая стоимость обновления | Узкий adapter layer внутри `codex_bridge` |
| Динамический GDScript | Ложная уверенность | Evidence/confidence model и runtime confirmation |
| Тяжёлый индекс в main thread | Подвисания редактора | Sidecar storage, background work, frame budget |
| Несохранённая сцена меняется во время действия | Неверное изменение | `scene_revision` и optimistic concurrency |
| Повреждение сцены write-инструментами | Потеря данных | EditorUndoRedoManager, atomic transaction, fault injection |
| Локальный endpoint раскрывает проект | Утечка кода/состояния | UDS/Named Pipe permissions, capability token, no public listener |
| Изменения MCP/app-server | Поломка клиентов | Version pinning, capability negotiation, compatibility tests |
| Несовпадение внешнего и встроенного поведения | Две разные интеграции | Один MCP и одна семантическая модель для всех surfaces |
| Размер больших проектов | Медленный startup | Incremental index, cache, pagination, lazy enrichment |
| Кроссплатформенный process lifecycle | Zombie processes и reconnect bugs | Platform abstraction и OS-specific integration tests |
| Непроверенная redistribution политика | Задержка релиза | Не предполагать bundling Codex до legal/license review |

## 15. Ветвление и выпуск

Рекомендуемая модель:

- `master` или отдельная sync branch отражает выбранную upstream-базу;
- `codex/integration` является основной integration branch;
- feature branches используют `codex/<area>-<short-name>`;
- release branch создаётся как `codex/release-1.0`;
- protocol/schema changes требуют changelog entry;
- несовместимое изменение до freeze повышает protocol minor/major по принятой policy;
- после S16 breaking changes запрещены без отдельного release exception.

Минимальные CI gates для merge:

- форматирование и static checks;
- Godot editor build затронутой платформы;
- bridge unit tests;
- protocol contract tests;
- MCP tests;
- fixture integration tests;
- security-sensitive tests для transport/auth/write изменений.

## 16. Рекомендуемый набор дочерних документов

Этот мастер-план рекомендуется декомпозировать в следующие документы:

Сквозное продуктовое видение и план семантического моста вынесены в [PRODUCT-001-semantic-bridge-vision-and-plan.md](PRODUCT-001-semantic-bridge-vision-and-plan.md); перечисленные ниже спецификации должны конкретизировать его в своих доменах.

Сквозной технический contract разделов 5.1–5.5 и issue-ready plan вынесены в [ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md](ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md). Он связывает перечисленные ниже ADR/specs, но не заменяет их wire/API детали.

1. `ADR-000-fork-and-upstream-strategy.md`
2. [ADR-001-component-boundaries-and-sidecar-language.md](ADR-001-component-boundaries-and-sidecar-language.md)
3. [PROTOCOL-001-bridge-rpc-v1.md](PROTOCOL-001-bridge-rpc-v1.md)
4. `MCP-001-tools-resources-and-approvals.md`
5. `INDEX-001-semantic-graph-and-storage.md`
6. `SCENE-001-scene-node-resource-model.md`
7. [SCRIPT-001-gdscript-and-csharp-adapters.md](SCRIPT-001-gdscript-and-csharp-adapters.md)
8. `EVIDENCE-001-confidence-and-source-model.md`
9. `EDITOR-001-live-editor-context.md`
10. `RUNTIME-001-debugger-and-runtime-observation.md`
11. `WRITE-001-editor-transactions-and-undo.md`
12. `VALIDATION-001-post-change-validation.md`
13. `UI-001-embedded-codex-client.md`
14. `SECURITY-001-threat-model.md`
15. `PERFORMANCE-001-budgets-and-benchmarks.md`
16. `TEST-001-fixtures-and-e2e-strategy.md`
17. `PLATFORM-001-macos-windows-linux.md`
18. [RELEASE-001-release-1.0-acceptance-and-evidence-plan.md](RELEASE-001-release-1.0-acceptance-and-evidence-plan.md)

Для каждого дочернего документа следует использовать одинаковый каркас:

- проблема и цели;
- scope/non-goals;
- термины;
- архитектура;
- публичные контракты;
- data flow;
- ошибки и recovery;
- безопасность;
- производительность;
- тестирование;
- миграции и совместимость;
- open questions;
- acceptance criteria.

## 17. Источники и исходные технические опоры

### Godot source

- `ResourceLoader::get_dependencies` — базовый источник зависимостей ресурсов: `core/io/resource_loader.cpp`; script-facing binding находится в `core/core_bind.cpp`.
- `EditorData::get_edited_scene_root` — доступ к открытой сцене: `editor/editor_data.h`.
- `EditorSelection::get_selected_nodes` — доступ к selection: `editor/editor_data.h`.
- `EditorPlugin::add_control_to_dock` — точка добавления встроенной панели: `editor/plugins/editor_plugin.h`.
- `EditorDebuggerNode` и debugger plugins — основа runtime-наблюдения: `editor/debugger/` и `editor/editor_node.cpp`.

### Codex official documentation

- Codex app-server предназначен для глубокого встраивания Codex в собственные клиенты, включая authentication, threads, approvals и streamed events: <https://learn.chatgpt.com/docs/app-server>.
- Project-scoped `.codex/config.toml` поддерживает настройку локальных MCP servers для доверенного проекта: <https://learn.chatgpt.com/docs/config-file/config-reference#configtoml>.

Ссылки и контрактные предположения проверены 2026-07-14. Поскольку app-server и отдельные Codex capabilities могут развиваться независимо от форка, их актуальность повторно проверяется в начале S11, S12, S16 и перед Stable gate; совместимость фиксируется точными версиями и сгенерированными schemas.

## 18. Управление изменениями этого roadmap

- Изменение порядка спринтов должно объяснять влияние на critical path.
- Новый обязательный scope 1.0 должен содержать оценку сроков и release risk.
- Acceptance criteria нельзя удалять только потому, что реализация их не проходит.
- После Sprint 6 изменения semantic entity model требуют migration plan.
- После Sprint 11 изменения MCP tools требуют compatibility assessment.
- После Sprint 16 действует API/scope freeze.
- Фактические результаты спринтов должны обновлять оценки следующих спринтов.

## 19. Следующее действие

[ADR-001](ADR-001-component-boundaries-and-sidecar-language.md), [PROTOCOL-001](PROTOCOL-001-bridge-rpc-v1.md) и канонический bundle `schemas/codex_bridge/v1` закрывают первый foundation-этап Sprint 1. Editor-only `modules/codex_bridge`, build guards, service/worker lifecycle и bounded main-thread dispatcher закрывают второй этап по локальному evidence [SPRINT-1-STAGE-2](SPRINT-1-STAGE-2.md). Private discovery, project-local lock, atomic runtime publication, права `0700`/`0600`, rotating token, macOS UDS framing и mutual HMAC handshake закрывают третий этап по локальному evidence [SPRINT-1-STAGE-3](SPRINT-1-STAGE-3.md). Authenticated request/response/cancel envelopes, lifecycle methods, deadlines, cancellation, exactly-once terminal arbitration и backpressure закрывают четвёртый этап по локальному evidence [SPRINT-1-STAGE-4](SPRINT-1-STAGE-4.md). Locked Rust conformance client, прямое использование канонических schemas/fixtures, cross-language discovery/handshake/lifecycle и negative transport suite, а также воспроизводимый redacted trace закрывают пятый этап по локальному evidence [SPRINT-1-STAGE-5](SPRINT-1-STAGE-5.md).

Локальная реализация и проверка Sprint 1 и Sprint 2 завершены. Sprint 3 принят: ResourceUID, прямой/обратный граф зависимостей, Bridge RPC 1.2, persistent segment index и два resource MCP tool прошли полный macOS/Windows evidence по [SPRINT-3-EVIDENCE](SPRINT-3-EVIDENCE.md); канонический D-05 выбирает segment store. Sprint 4 также принят: structural `PackedScene`/`SceneState` index, Bridge RPC 1.3, `segment-v2` и два scene MCP tool прошли единый macOS/Windows freeze по [SPRINT-4-EVIDENCE](SPRINT-4-EVIDENCE.md). Sprint 5 начат: `S5-01` локально закрывает D-07 и фиксирует [SCRIPT-001](SCRIPT-001-gdscript-and-csharp-adapters.md), `S5-02` фиксирует независимый script fixture/golden oracle, а `S5-03` — compatible Bridge RPC 1.4 profile; следующий gate — `ScriptSemanticAdapter` и bounded journal `S5-04` по [SPRINT-5-PLAN](SPRINT-5-PLAN.md). Публикация и remote CI выполняются только по отдельной авторизации.
