# RELEASE-001 — Приёмка, доказательства и план выпуска Godot × Codex 1.0

**Статус:** Draft for review 0.1
**Дата:** 2026-07-14
**Целевой релиз:** 1.0
**Родительские документы:** [MASTER_SPRINT_ROADMAP.md](MASTER_SPRINT_ROADMAP.md), разделы 3, 8, 11 и 13; [PRODUCT-001-semantic-bridge-vision-and-plan.md](PRODUCT-001-semantic-bridge-vision-and-plan.md)
**Владельцы решения:** Release, Product, Engine/Editor, Sidecar/Protocol, Codex Client, QA, Security и Platform — назначаются до завершения Sprint 1

---

## 1. Назначение и границы документа

Этот документ превращает определение релиза 1.0 из раздела 3 мастер-плана в исполнимую программу приёмки. Он задаёт:

- однозначное правило решения `go/no-go`;
- идентификаторы и критерии прохождения десяти обязательных сценариев;
- матрицу поверхностей Codex, платформ, типов тестов и доказательств;
- измерение метрик раздела 8;
- накопление evidence package для gates раздела 11;
- реализационный план release-readiness работ поверх Sprint 0–17;
- правила для known limitations, исключений и повторного тестирования.

Документ не заменяет архитектуру, wire schemas или реализацию отдельных подсистем. Их определяют `PROTOCOL-001`, `MCP-001`, `INDEX-001`, `SCENE-001`, `SCRIPT-001`, `EDITOR-001`, `RUNTIME-001`, `WRITE-001`, `VALIDATION-001`, `TEST-001`, `PERFORMANCE-001` и `PLATFORM-001`.

При расхождении документов применяется следующий порядок:

1. мастер-план определяет обязательный scope 1.0, milestone и release gates;
2. `PRODUCT-001` определяет продуктовые инварианты и семантическую модель;
3. этот документ определяет способ доказательства готовности 1.0;
4. дочерние спецификации определяют конкретные API, fixtures и команды запуска;
5. расхождение устраняется изменением документов и traceability matrix, а не ослаблением теста или скрытым release exception.

## 2. Нормативное правило выпуска 1.0

Решение о Stable 1.0 является конъюнкцией всех обязательных условий:

```text
Stable 1.0 = R1-01 ∧ R1-02 ∧ ... ∧ R1-10
             ∧ все release-метрики раздела 8
             ∧ Stable gate раздела 11.4
             ∧ полный и актуальный evidence package
```

Следствия этого правила:

- провал одного обязательного сценария оставляет сборку в состоянии pre-1.0;
- unit test сам по себе не закрывает ни один сценарий `R1-*`;
- отсутствие evidence приравнивается к непрохождению требования;
- `not run`, `partial`, `flaky`, `blocked` и `pass with missing evidence` не являются `pass`;
- known limitation не может исключить обязательную поверхность, платформу, semantic capability или безопасный Undo/recovery path;
- номер версии, календарная дата и процент закрытых задач не заменяют gate review;
- Stable принимается только на финальных распространяемых artifacts, а не на локальной developer build.

### 2.1. Обязательные продуктовые инварианты

Каждый применимый сценарий обязан доказать одновременно:

1. **Без ручной передачи контекста.** Пользователь не копирует scene tree, Inspector values, логи, stack trace или screenshot в запрос.
2. **Project binding.** Ответ и операция относятся к доказанному `project_id` и не смешиваются с другим открытым проектом.
3. **Разделение состояний.** Disk/static, live editor, runtime и operation state имеют отдельные источники и revision/session coordinates.
4. **Evidence-backed semantics.** Существенные факты имеют source, location, confidence, freshness и revision.
5. **Честная неопределённость.** Динамический или stale факт не маркируется как текущий `exact`.
6. **Единый контракт.** App, CLI, поддерживаемая IDE-интеграция и Godot Dock используют одну MCP schema и одну каноническую semantic model.
7. **Транзакционная запись.** Write имеет preconditions, preview, approval, transaction ID, validation и полный Undo/recovery path.
8. **Fail-safe recovery.** Restart, disconnect, timeout или corrupt cache не повреждают проект и не вызывают слепой повтор write.
9. **Воспроизводимость.** Evidence привязан к commit, версиям schemas, fixture, ОС и hash artifact.
10. **Самостоятельная эксплуатация.** Установка, update, doctor, recovery и uninstall выполняются по опубликованной документации.

### 2.2. Языковой scope

Для сцен, ресурсов и GDScript требуется полный scope сценариев `R1-01`–`R1-10`. Проверка GDScript включает symbols, resolved references, scene attachment, diagnostics и корректную классификацию динамических связей.

Для C# обязательный minimum 1.0:

- обнаружение `.cs` scripts и их identity;
- связь script ↔ scene/node;
- diagnostics из поддерживаемого language-server пути;
- symbols/references, которые language server способен предоставить в заявленной compatibility matrix;
- честный `capability_unavailable` или сниженный confidence там, где глубокая семантика недоступна.

Полный Roslyn-паритет не является требованием 1.0, но отсутствие любого пункта обязательного C# minimum блокирует Stable. Поддерживаемая C# toolchain/LSP version фиксируется до Beta gate.

## 3. Реестр обязательных требований

| ID | Пункт §3 | Обязательный результат | Legacy-сценарий | Основные спринты | Минимальный уровень доказательства |
|---|---:|---|---|---:|---|
| `R1-01` | 1 | Текущая сцена, selection и несохранённые свойства | A | S2, S7 | Integration + E2E |
| `R1-02` | 2 | Find usages для resource/scene/node/signal/GDScript symbol с evidence | B | S3–S6 | Golden + integration + E2E |
| `R1-03` | 3 | Объяснение структуры сцены и зависимостей | C | S3–S7 | Golden + E2E rubric |
| `R1-04` | 4 | Run, runtime tree, diagnostics, stack trace и viewport capture | D, runtime-наблюдение | S8 | Runtime E2E |
| `R1-05` | 5 | Диагностика через корреляцию files/editor/runtime | D, diagnosis | S6–S8 | Model E2E + deterministic trace checks |
| `R1-06` | 6 | Подготовка безопасного editor-native изменения сцены | E, prepare/preview | S9–S10 | Transaction integration + E2E |
| `R1-07` | 7 | Preview → approval → apply → validation → Undo | E, apply/Undo | S9–S10, S13 | Fault injection + E2E + manual UX |
| `R1-08` | 8 | Семантический паритет App/CLI/IDE/Dock | H, I | S11–S13 | Contract parity + surface E2E |
| `R1-09` | 9 | Restart/reconnect/recovery без повреждения проекта | F | S1, S7, S9, S12, S14 | Process/fault E2E |
| `R1-10` | 10 | Install/update/doctor/uninstall на macOS, Windows и Linux | J | S11, S14–S17 | Platform E2E + manual acceptance |

Сценарий G из мастер-плана, multi-project isolation, является обязательным сквозным security-инвариантом для `R1-01`–`R1-09`, хотя не образует отдельный пользовательский пункт раздела 3.

## 4. Общие условия acceptance run

### 4.1. Зафиксированная тестовая единица

Каждый run фиксирует:

- commit Godot fork, bridge module и sidecar;
- версии Codex surfaces и app-server;
- версии Bridge RPC, MCP schema, semantic index и transaction schema;
- artifact hash и способ установки;
- OS image, architecture, toolchain и filesystem characteristics;
- fixture/real-project revision и исходный project hash;
- `project_id`, `editor_session_id`, применимый `runtime_session_id` и начальный revision vector;
- surface, consistency mode и approval policy;
- test harness version и идентификатор test case.

Неидентифицированная локальная сборка не создаёт release evidence.

### 4.2. Канонические fixture projects

`TEST-001` обязан определить минимум четыре класса fixtures:

| Fixture | Назначение |
|---|---|
| `semantic-golden` | UID rename, resource dependencies, inheritance, instances, overrides, signals, groups, animation paths, GDScript и C# minimum |
| `live-editor` | Несохранённые properties/structure, multi-selection, несколько scene tabs, external disk conflict, native Undo/Redo |
| `runtime-diagnostic` | Remote tree, deliberate warning/error, nested stack, runtime-only node/property и viewport marker |
| `transaction-recovery` | Простая и составная scene/script transaction, validation failure, disconnect points, corrupt cache и multi-project isolation |

Reference-medium и reference-large projects определяются в `PERFORMANCE-001`. До RC дополнительно используются минимум три репрезентативных реальных проекта согласно Sprint 16.

### 4.3. Запрет ручной подмены контекста

Тест считается недействительным, если prompt содержит сведения, которые интеграция должна была получить сама: выбранный NodePath, Inspector value, scene tree, runtime log, stack frame, resource owner list или screenshot. Prompt может содержать только пользовательскую цель и явно допустимые уточнения.

Harness сохраняет исходный prompt и список MCP/tool calls. Проверка должна показать, что ответ выведен из наблюдаемого контекста текущей project/editor/runtime session.

## 5. Спецификация сценариев `R1-*`

### 5.1. `R1-01` — Live selection и unsaved editor state

**Предусловия**

- fixture открыт в специальной сборке Godot;
- на диске сохранено контрольное значение property;
- пользователь выбирает известный node и меняет property без save;
- sidecar подключён к тому же canonical project root.

**Действия**

1. Из Codex запрашивается текущая сцена и selection без указания NodePath/value в prompt.
2. Получается editor snapshot и evidence для выбранного объекта.
3. Выполняется повторный запрос после ещё одного несохранённого изменения.

**Pass criteria**

- возвращены точные scene identity, node identity/NodePath, type, owner, attached script и live property value;
- disk value и editor value не смешаны; dirty state указан явно;
- присутствуют `project_id`, `editor_session_id`, `scene_revision` и freshness;
- second response отражает новую revision и не переиспользует прежнее значение как current;
- evidence source является live editor/Inspector observation;
- ни один другой открытый проект не влияет на результат.

**Evidence:** protocol trace, normalized MCP response, pre/post editor snapshot, E2E assertion и короткий UX capture.

### 5.2. `R1-02` — Find usages с доказательствами

**Предусловия**

Fixture содержит известные exact, probable, dynamic и runtime-confirmed usages для:

- resource по UID;
- scene и instance;
- node/node path;
- signal declaration/connection/use;
- GDScript symbol;
- C# minimum, где usage доступен через заявленный language server.

**Действия**

1. Для каждого entity kind выполняется единый find-usages query.
2. Проверяются фильтры confidence, scope и pagination.
3. Ресурс переименовывается с сохранением UID, после чего query повторяется.

**Pass criteria**

- каждое usage содержит entity IDs, relation, source location и revision-aware evidence;
- exact results совпадают с golden graph; ложных `exact` нет;
- probable/dynamic results отделены от exact и не теряются при фильтрации;
- rename ресурса не разрывает identity и обновляет reverse usages;
- static resolvable recall соответствует метрике раздела 8;
- duplicate provenance агрегируется без потери источников.

**Evidence:** golden graph diff, query trace, accuracy report с numerator/denominator и E2E navigation proof.

### 5.3. `R1-03` — Scene comprehension

**Предусловия**

Scene fixture включает inheritance, nested instances, local overrides, external/subresources, signals, groups, animation `NodePath`, attached scripts и одну dirty live override.

**Действия**

Пользователь просит объяснить структуру текущей сцены, происхождение значимых свойств и зависимости, не передавая `.tscn` или scene tree вручную.

**Pass criteria**

- source scene, inherited content, instances, editable children и local overrides различены;
- dependencies связаны с UID/entity identities и evidence;
- signals, groups и animation paths показаны либо честно отмечены broken/unresolved;
- live overlay имеет приоритет над сохранённой проекцией только в покрываемых полях;
- runtime instance не выдан за source node definition;
- обязательные факты ответа присутствуют в underlying structured result, а не только в prose модели.

**Evidence:** canonical scene graph, structured fact set, model answer, rubric result и ссылки answer → evidence IDs.

### 5.4. `R1-04` — Runtime observation

**Предусловия**

Runtime fixture воспроизводимо создаёт известное дерево, диагностическое сообщение, stack trace и визуальный viewport marker.

**Действия**

1. Codex запускает current scene или project через bridge.
2. Запрашивает runtime tree и выбранный runtime object.
3. Воспроизводит deliberate error и получает stack trace.
4. Делает viewport capture.
5. Останавливает и повторно запускает игру.

**Pass criteria**

- run lifecycle управляется редактором и имеет structured status;
- tree, object properties, diagnostics, stack и image относятся к одному `runtime_session_id`;
- stack frame связан с source symbol/location с evidence и корректным confidence;
- screenshot относится к зафиксированной runtime revision/session и проходит size/redaction limits;
- новый run создаёт новый session ID; старые objects не возвращаются как current;
- crash/hang game process не блокирует bridge/editor.

**Evidence:** runtime event trace, tree/stack snapshot, viewport artifact с metadata, session reset assertion и E2E report.

### 5.5. `R1-05` — Сквозная диагностика files + editor + runtime

**Предусловия**

Fixture содержит проблему, для которой одного файла недостаточно: например, сохранённое default value отличается от dirty Inspector override, а runtime error указывает на связанный symbol.

**Действия**

Пользователь просит диагностировать проблему обычным языком без ручной вставки editor/runtime данных.

**Pass criteria**

- trace показывает обращения к необходимым static, live editor и runtime sources;
- вывод различает наблюдение, inference и неопределённость;
- root-cause hypothesis соответствует golden rubric;
- все ключевые утверждения имеют evidence IDs и согласованный revision vector;
- конфликт revisions или недостаток capability возвращается явно, а не заполняется догадкой;
- предложенное исправление указывает правильные affected entities и не применяется без отдельной transaction flow.

Текстовая убедительность ответа без deterministic проверки tool trace и evidence не считается достаточной.

**Evidence:** исходный prompt, model/tool trace, normalized facts, rubric score, expected root cause и reviewer decision.

### 5.6. `R1-06` — Подготовка безопасного изменения сцены

**Предусловия**

Открыта dirty scene с известными editor/history revisions. Change request затрагивает node structure/property и при необходимости script/signal.

**Действия**

1. Codex читает consistent snapshot.
2. Формирует semantic change set.
3. Запрашивает preview, не изменяя scene.
4. До approval выполняется конкурирующий edit для отдельного stale-precondition test.

**Pass criteria**

- change set имеет `transaction_id`, idempotency key, preconditions, affected entities/files, risk и validation plan;
- preview отделяет scene operations от file diff и показывает значения до/после;
- до approval проект и native history не изменены;
- stale revision отклоняется до apply и требует нового preview;
- открытая scene не модифицируется raw `.tscn` write;
- unsupported operation возвращает structured error без частичного изменения.

**Evidence:** pre/post project hashes до approval, transaction plan/preview, stale conflict trace и editor history snapshot.

### 5.7. `R1-07` — Approval, apply, validation и полный Undo

**Действия**

1. Пользователь просматривает semantic preview.
2. Явно подтверждает подготовленную transaction.
3. Bridge применяет change set через editor-native operation path.
4. Выполняются diagnostics и предусмотренный validation run.
5. Пользователь вызывает Undo.

**Pass criteria**

- apply возможен только для одобренной и неустаревшей transaction;
- все operations применены атомарно либо полностью rolled back;
- post-apply revisions, editor history и semantic graph отражают изменение;
- validation report содержит diagnostics, affected entities и итоговый status;
- один заявленный Undo path восстанавливает исходную scene/script state и semantic graph;
- повторный apply с тем же idempotency key не дублирует изменение;
- disconnect в каждой fault point приводит к `prepared`, `in_doubt`, `rolled_back` или иному документированному состоянию, но не к тихому partial success.

**Evidence:** approval record, journal, native history, fault-injection matrix, pre/apply/undo graph diff, project hashes и UX capture.

### 5.8. `R1-08` — Паритет поверхностей Codex

**Обязательные поверхности**

1. Codex App;
2. Codex CLI;
3. одна явно названная и версионированная поддерживаемая IDE-интеграция;
4. Godot Codex Dock на базе Codex app-server.

**Действия**

На одном логически эквивалентном fixture snapshot каждая поверхность выполняет:

- project overview/status;
- `R1-01` live selection;
- `R1-02` find usages;
- `R1-04` runtime diagnosis slice;
- подготовку, preview, approval и Undo одной `R1-06`/`R1-07` transaction;
- structured error для stale revision и unavailable capability.

**Pass criteria**

- все поверхности используют одну versioned MCP schema и project-scoped configuration;
- ни одна поверхность не имеет отдельного Godot indexer, entity namespace или write bypass;
- после canonical normalization совпадают entity IDs, facts, evidence, confidence, revision vectors, errors, transaction preconditions, preview semantics и validation result;
- разрешены различия только в presentation, navigation, streaming и UI layout;
- Dock использует app-server для Codex lifecycle, но получает Godot-семантику через тот же MCP contract;
- version/capability mismatch имеет явный remediation path.

Codex App, CLI и IDE на одном host разделяют Codex configuration layers; project MCP config загружается только для trusted project. Release test обязан учитывать trust/setup state, а не полагаться на неявную пользовательскую конфигурацию.

**Evidence:** canonical response bundle по каждой поверхности, semantic diff, config/versions manifest, tool trace и UX checklist.

### 5.9. `R1-09` — Restart, reconnect и recovery

**Fault points**

- sidecar restart во время read;
- sidecar restart после preview, до apply;
- disconnect во время apply;
- Godot/editor controlled restart после committed transaction;
- app-server restart при открытом Dock thread;
- event sequence gap;
- несовместимый или повреждённый index cache;
- stale discovery record/token.

**Pass criteria**

- новый editor/runtime process получает новые session IDs;
- sidecar выполняет delta replay только при доказанной непрерывности, иначе full resync;
- prepared transaction не меняет проект;
- transaction `in_doubt` сверяется по ID, revisions и editor history без слепого retry;
- committed change сохраняет корректный Undo/recovery status согласно документированному lifecycle;
- corrupt cache изолируется и воспроизводимо перестраивается;
- stale token/discovery не открывает доступ к новой session;
- scene/resource/script contents не повреждены; контрольные hashes и Godot load validation проходят;
- пользователь получает конкретные status/remediation instructions.

**Evidence:** process timeline, fault injector output, session/revision trace, pre/post hashes, project-open validation и recovery checklist.

### 5.10. `R1-10` — Platform installation lifecycle

Сценарий выполняется отдельно на чистых поддерживаемых образах macOS, Windows и Linux.

**Действия на каждой ОС**

1. Проверить artifact provenance, checksum/signature и prerequisites.
2. Установить специальную сборку Godot, bridge/sidecar и необходимые Codex integration assets по документации.
3. Подключить trusted fixture project без ручного редактирования скрытых внутренних файлов, кроме документированного setup path.
4. Выполнить doctor и smoke workflow `R1-01`, `R1-04`, `R1-07` и `R1-09`.
5. Обновиться с последней поддерживаемой Beta/RC или указанной предыдущей версии.
6. Повторить doctor и smoke workflow.
7. Удалить интеграцию.
8. Проверить сохранность project files, Codex history согласно policy и отсутствие orphan processes/listeners.

**Pass criteria**

- clean install, update, doctor и uninstall завершаются по опубликованной процедуре;
- пути с пробелами и Unicode проходят; на Windows проверяется заявленный long-path behavior;
- transport permissions и process lifecycle соответствуют платформе;
- doctor различает missing binary, stale discovery, version mismatch, config/trust/auth issue и corrupt cache;
- update либо мигрирует совместимые index/config data, либо безопасно перестраивает cache;
- uninstall не удаляет пользовательский project, scenes, scripts или неоговорённую Codex history;
- package manifest, provenance и hashes совпадают с опубликованными;
- smoke workflow семантически эквивалентен на трёх ОС.

**Evidence:** platform CI/E2E logs, signed manual checklist, installer/update/uninstaller logs, doctor reports, artifact hashes и post-uninstall filesystem/process audit.

## 6. Матрица поверхностей и платформ

### 6.1. Обязательная матрица

| Измерение | Stable 1.0 requirement |
|---|---|
| Godot integration | Полный поддерживаемый workflow на macOS, Windows и Linux |
| Codex App | `R1-08` на каждой ОС, где конкретная зафиксированная версия App официально поддерживается |
| Codex CLI | `R1-08` на macOS, Windows и Linux |
| Поддерживаемая IDE-интеграция | `R1-08` на macOS, Windows и Linux для объявленной IDE/version matrix |
| Godot Dock | `R1-08` на macOS, Windows и Linux |
| Packaging | Полный `R1-10` на всех трёх ОС |

До завершения Sprint 11 владельцы Product/Release обязаны зафиксировать upstream support matrix Codex App и выбранной IDE. Если обязательная поверхность недоступна на целевой ОС по независящей от проекта причине, это оформляется как открытый scope decision до Beta gate. Нельзя молча отметить ячейку `N/A`: либо требование раздела 3 формально уточняется, либо Stable остаётся заблокированным.

### 6.2. Что сравнивается при semantic parity

Canonical comparator игнорирует только presentation-only поля: порядок UI, локализованный текст, timestamps transport delivery и streaming chunk boundaries. Он обязан сравнивать:

- schema/status/capabilities;
- project/session/revision coordinates;
- entity identities и relation facts;
- evidence, confidence и freshness;
- diagnostics и structured errors;
- transaction preconditions, operations и risk classification;
- semantic preview и validation result;
- truncation/pagination semantics.

## 7. Измерение release-метрик

Метрики из раздела 8 мастер-плана измеряются по единому протоколу:

| Метрика | Метод измерения | Stable threshold |
|---|---|---:|
| False `exact` | Все `exact` facts на versioned golden fixtures вручную/структурно сверяются с truth set | 0 |
| Static usage recall | `true positive / all statically resolvable expected usages`; dynamic cases исключаются из denominator и учитываются отдельно | ≥ 95% |
| Cached read latency | Warm compatible index, reference-medium, не менее согласованного числа запросов; client overhead публикуется отдельно | p95 ≤ 300 мс |
| Current scene latency | Ready bridge/sidecar, live editor, response до получения полного normalized result | p95 ≤ 500 мс |
| Incremental visibility | От принятого file/editor event до query, возвращающего новую revision | p95 ≤ 2 с |
| Editor stalls | Instrumented main-thread budget на reference projects | 0 stalls сверх бюджета `PERFORMANCE-001` |
| Scene integrity | Fault matrix + load/parse/hash/semantic validation | 0 повреждений |
| Write traceability | Все попытки apply в release suite | 100% имеют transaction ID и документированный Undo/recovery path |
| Network boundary | Port/socket/process inspection на трёх ОС | 0 non-loopback listeners |
| Reconnect | Все обязательные fault cases `R1-09` | 100% pass |
| Clean install | `R1-10` на clean OS images | 3 из 3 ОС pass |

Правила измерения, размер выборки, hardware profiles, warm-up и raw data фиксируются в `PERFORMANCE-001`. Если бюджет editor frame stall не определён численно до Beta, соответствующая метрика считается непроверенной и gate не проходит.

## 8. Стратегия тестирования и допустимые доказательства

### 8.1. Test layers

| Layer | Что доказывает | Может самостоятельно закрыть `R1-*` |
|---|---|---:|
| Unit | Локальная логика adapter/index/merge/redaction | Нет |
| Schema/contract | Совместимость Bridge RPC/MCP/app-server adapter | Нет |
| Golden/benchmark | Accuracy, identity, graph и performance на truth set | Нет |
| Integration | Реальное взаимодействие bridge ↔ sidecar ↔ MCP | Только вместе с E2E/другим требуемым layer |
| Process/fault E2E | Lifecycle, crash, reconnect, transaction recovery | Да для соответствующей части |
| Surface E2E | Реальный путь App/CLI/IDE/Dock | Да при наличии machine evidence |
| Manual acceptance | UX, installer, accessibility, визуальный preview | Да только вместе с machine-readable trace там, где проверяется семантика |

### 8.2. Минимальный evidence package одного run

Каждая запись содержит:

```json
{
  "run_id": "run:01J...",
  "requirement_ids": ["R1-07"],
  "scenario_id": "E",
  "result": "pass",
  "started_at": "2026-07-14T10:00:00Z",
  "surface": "codex_cli",
  "platform": "macos-arm64",
  "components": {
    "godot_commit": "<sha>",
    "bridge_version": "<version>",
    "sidecar_version": "<version>",
    "codex_version": "<version>",
    "mcp_schema": "<version>"
  },
  "artifact_hashes": ["sha256:<hash>"],
  "fixture": { "id": "transaction-recovery", "revision": "<sha>" },
  "revision_vector_before": {},
  "revision_vector_after": {},
  "assertions": [],
  "artifacts": [],
  "defects": [],
  "reviewer": "<owner>"
}
```

Полный schema определяется в `TEST-001`. Logs и artifacts проходят redaction policy; секреты, абсолютные приватные пути и содержимое реальных проектов не включаются без отдельного opt-in.

### 8.3. Актуальность и инвалидация evidence

Evidence повторяется, если после run изменился любой влияющий компонент:

- release artifact или relevant source commit;
- Bridge RPC/MCP/transaction schema;
- semantic adapter, index migration или evidence/confidence logic;
- surface/app-server adapter;
- installer/update/uninstaller;
- fixture truth set или benchmark protocol;
- OS image/toolchain в части, затрагивающей platform result.

Impact analysis может ограничить повторный запуск незатронутыми сценариями только до RC. Stable gate всегда повторяет полный обязательный suite на финальных artifacts.

### 8.4. Flaky policy

- первый нестабильный результат открывает defect и помечает scenario `flaky`, а не `pass`;
- автоматический retry сохраняет все попытки и причину retry;
- gate evidence требует согласованного числа последовательных успешных прогонов, определённого `TEST-001`;
- quarantine обязательного `R1-*` теста блокирует соответствующий gate.

## 9. Traceability к release gates

| Gate | Обязательное покрытие | Допустимые ограничения |
|---|---|---|
| Alpha после S8 | `R1-01`–`R1-05` на macOS через внешний Codex; security/reconnect foundation | Write, Dock и cross-platform packaging ещё не обязательны |
| Beta после S15 | `R1-01`–`R1-10` на fixtures; полный read/runtime/write path; surface/platform matrix; метрики hardening | High defects допустимы только с владельцем до RC и не могут считаться Stable pass |
| RC readiness после S16 | Все `R1-*` на трёх ОС и минимум трёх реальных проектах; frozen schemas; metrics; install/upgrade | Только письменные exceptions, не отменяющие обязательный сценарий |
| Stable после S17 | Полный повтор на финальных подписанных artifacts, stabilization window и архив evidence | Только medium/low defects вне обязательных сценариев, security, integrity и compatibility |

Gate review использует manifest, а не ручной список ссылок. Manifest обязан показать каждую required cell и одно из состояний `pass/fail/not_run/flaky/blocked`; только `pass` удовлетворяет gate.

## 10. План реализации release readiness

План не создаёт второй календарь и не меняет Sprint 0–17. Он выделяет сквозные release work packages, которые должны выполняться одновременно с продуктовой реализацией.

### 10.1. Этапы

| Этап | Спринты | Release-readiness результат | Exit evidence |
|---|---:|---|---|
| `RA-0` Requirements baseline | S0–S1 | Утверждены `R1-*`, owners, traceability schema и support-matrix decision process | Reviewed RELEASE-001 + initial manifest |
| `RA-1` Vertical evidence slice | S2 | `R1-01` проходит через bridge → sidecar → MCP → внешний Codex | Archived M0 trace |
| `RA-2` Semantic acceptance core | S3–S6 | Truth sets, accuracy metric и `R1-02`/`R1-03` harness | Golden/benchmark report |
| `RA-3` Live/runtime acceptance | S7–S8 | Revision conflicts, runtime sessions и `R1-04`/`R1-05` E2E | Alpha evidence package |
| `RA-4` Transaction safety | S9–S10 | `R1-06`/`R1-07`, fault points и full Undo доказаны | Transaction/fault report |
| `RA-5` Surface parity | S11–S13 | App/CLI/IDE/Dock comparator и UX acceptance | `R1-08` parity bundle |
| `RA-6` Production/platform readiness | S14–S15 | Metrics, recovery, installers и three-OS matrix | Beta evidence package |
| `RA-7` Real-project RC | S16 | Frozen scope/schemas, реальные проекты и documentation usability | RC readiness package |
| `RA-8` Final audit | S17 | Полный rerun на final artifacts и подписанный go/no-go | Stable evidence archive |

### 10.2. Сквозные work packages

#### `WP-REQ` — Requirements и traceability

- поддерживать mapping `R1-*` → master scenario → sprint → component → test case → evidence → defect;
- версионировать acceptance criteria;
- автоматически обнаруживать required cells без evidence;
- блокировать release manifest при `fail/not_run/flaky/blocked`.

#### `WP-FIX` — Fixtures и truth sets

- создать четыре canonical fixture classes из раздела 4.2;
- хранить expected entities/relations/evidence отдельно от output реализации;
- добавить mutations: rename, reparent, dirty override, external conflict, new runtime session, transaction failure;
- провести независимый review truth sets Engine/QA владельцами.

#### `WP-HARNESS` — Contract, E2E и parity harness

- реализовать protocol recorder и canonical JSON normalizer;
- запускать реальные processes и surfaces с фиксированным project root;
- сравнивать semantic output, а не prose/formatting;
- сохранять revision/session timeline и redacted tool trace.

#### `WP-METRIC` — Accuracy и performance

- определить denominator static resolvable usages;
- автоматизировать false-`exact` audit;
- создать reference hardware/project profiles;
- собирать latency, index freshness и editor main-thread telemetry;
- публиковать raw aggregate и p50/p95 без project content.

#### `WP-TXN` — Integrity и recovery

- внедрить fault injection во все transaction lifecycle boundaries;
- проверять Godot loadability, semantic graph и content hashes после failure/Undo;
- покрыть `in_doubt`, idempotency и reconnect reconciliation;
- выполнить multi-project negative access tests.

#### `WP-SURFACE` — Клиентский паритет

- зафиксировать supported Codex App/CLI/IDE/app-server versions;
- подтвердить один project-scoped MCP contract;
- создать adapter для запуска одинакового scenario trace на четырёх surfaces;
- отделить semantic parity assertions от UX checklist.

#### `WP-PLATFORM` — Packaging и operations

- создать clean OS images/runners;
- автоматизировать install/update/doctor/uninstall;
- проверить permissions, paths, signatures и process cleanup;
- подготовить platform-specific troubleshooting и rollback.

#### `WP-EVIDENCE` — Gate automation

- определить machine-readable manifest и artifact retention;
- формировать Alpha/Beta/RC/Stable dashboards из test output;
- связывать failures с defects и release exceptions;
- подписывать и архивировать Stable package рядом с release manifest.

### 10.3. Ближайший исполнимый backlog

| Приоритет | Задача | Владелец роли | Дедлайн |
|---:|---|---|---:|
| P0 | Утвердить `R1-*` IDs и conflict order документов | Product + Release | S1 |
| P0 | Назначить владельца каждого `R1-*` и release metric | Engineering + QA | S1 |
| P0 | Создать `TEST-001` со schema evidence manifest | QA/Protocol | S1 |
| P0 | Определить Codex surface/version и IDE support matrix | Codex Client + Release | До S11, initial в S1 |
| P0 | Создать skeleton `semantic-golden` и `live-editor` fixtures | Engine + QA | S2 |
| P0 | Реализовать protocol recorder/canonical normalizer | Protocol/QA | S2 |
| P0 | Заархивировать первый полный `R1-01` M0 trace | Engine + Sidecar + QA | S2 |
| P1 | Определить accuracy truth-set review и recall denominator | Index + QA | S3 |
| P1 | Добавить `R1-02`/`R1-03` golden harness | Index/Scene/Script | S6 |
| P1 | Создать runtime/diagnosis rubric и deterministic trace assertions | Runtime + QA | S8 |
| P1 | Создать transaction fault matrix и integrity oracle | Write + QA | S9 |
| P1 | Автоматизировать pre/apply/undo semantic graph diff | Write/Index | S10 |
| P1 | Реализовать four-surface parity adapter | Codex Client + QA | S13 |
| P1 | Зафиксировать numeric frame budget и benchmark protocol | Performance | До S14 gate |
| P1 | Автоматизировать clean platform lifecycle | Platform/Release | S15 |
| P1 | Создать gate dashboard и missing-evidence check | Release/QA | S15 |
| P2 | Провести documentation usability test без помощи разработчика | Docs/UX/QA | S16 |
| P2 | Выполнить полный final-artifact rerun и подписать manifest | Все gate owners | S17 |

## 11. Ответственность и go/no-go

| Роль | Ответственность |
|---|---|
| Product owner | Подтверждает, что pass criteria сохраняют пользовательский смысл раздела 3 |
| Engineering owner | Подтверждает версии, commits, fixes и отсутствие скрытого bypass |
| QA owner | Владеет truth sets, test independence, flakiness и evidence manifest |
| Security reviewer | Проверяет isolation, token/listener/redaction и security defects |
| Platform owner | Подписывает install/update/doctor/uninstall evidence трёх ОС |
| Codex client owner | Подтверждает surface/version matrix, MCP parity и app-server adapter |
| Release owner | Проверяет полноту manifest, exceptions и принимает итоговый `go/no-go` процесс |

Ни один владелец не может единолично принять Stable. Требуются подписи Release, Engineering, QA и Security, как определено Stable gate мастер-плана.

## 12. Known limitations и release exceptions

Допустимый known limitation:

- уточняет границу dynamic analysis;
- описывает presentation/UX несовершенство;
- касается неподдерживаемой сторонней версии вне compatibility matrix;
- имеет безопасный workaround;
- не нарушает один из `R1-*`, product invariant, security boundary или data integrity.

Недопустимый known limitation:

- исключает обязательную поверхность или одну из трёх ОС;
- разрешает ручное копирование обязательного контекста;
- допускает ложный `exact`;
- заменяет editor transaction raw file edit;
- убирает preview, approval, validation или Undo;
- разрешает потерю/повреждение project data;
- подменяет integration/E2E evidence unit test;
- объявляет `flaky` обязательный сценарий пройденным.

Release exception содержит owner, defect, scope, risk, workaround, expiry и affected requirements. Исключение для `R1-*` может быть допустимо в Beta/RC только как временный план исправления; оно не превращает Stable failure в pass.

## 13. Open decisions и сроки

| Решение | Владелец | Срок | Блокирует |
|---|---|---:|---|
| Точная поддерживаемая IDE и version range | Codex Client | До начала S11 | `R1-08`, Beta |
| Codex App platform support matrix для release artifacts | Product/Release | До начала S11 | Matrix 6.1, Beta |
| Evidence manifest format и retention | QA/Security | До завершения S1 | Все gates |
| Golden truth-set review process | QA/Engine | До S3 acceptance | `R1-02`, `R1-03` |
| Numeric editor frame budget и benchmark hardware | Performance | До начала S14 | Метрики, Beta |
| Supported C# language server/toolchain | Script/Platform | До Beta | C# minimum |
| Upgrade source versions для `R1-10` | Release/Platform | До S15 acceptance | Beta/RC |
| Minimum consecutive runs для flaky-sensitive cases | QA | До Alpha | Gate evidence |

Пропущенный срок переводит зависимое требование в `blocked`; он не создаёт значение по умолчанию.

## 14. Критерии утверждения RELEASE-001

Документ может перейти в `Approved`, когда:

- каждый пункт раздела 3 мастер-плана имеет ровно один основной `R1-*` ID;
- Product и QA согласовали pass criteria без ослабления scope;
- Engine/Sidecar/Client owners подтвердили собираемость обязательных evidence;
- Platform owner принял `R1-10` и three-OS lifecycle;
- Security reviewer принял evidence retention/redaction и recovery assertions;
- `TEST-001` ссылается на `R1-*` и определяет manifest schema;
- master roadmap содержит сценарий J и ссылку на этот документ;
- все open decisions имеют владельца и срок.

## 15. Источники и проверенные опоры

### 15.1. Документы проекта

- [MASTER_SPRINT_ROADMAP.md](MASTER_SPRINT_ROADMAP.md) — обязательный scope, метрики, Sprint 0–17, gates и acceptance scenarios.
- [PRODUCT-001-semantic-bridge-vision-and-plan.md](PRODUCT-001-semantic-bridge-vision-and-plan.md) — state/revision/evidence model, semantic parity, transactions и recovery.
- [ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md](ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md) — component contract и реализационный план bridge, sidecar, индекса и evidence.

### 15.2. Официальная документация Codex

- [Model Context Protocol](https://learn.chatgpt.com/docs/extend/mcp) — локальные Codex App, CLI и IDE поддерживают MCP и разделяют MCP configuration на одном host; project config применяется только для trusted project.
- [Codex App Server](https://learn.chatgpt.com/docs/app-server) — app-server предназначен для deep client integration, включая authentication, threads, approvals и streamed agent events; generated schemas привязаны к конкретной версии Codex.

Контрактные предположения проверены 2026-07-14. Они повторно проверяются перед S11, S12, S16 и Stable gate; release manifest фиксирует фактически протестированные версии.
