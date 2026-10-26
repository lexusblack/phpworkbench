# PLAN — от Zed к phpworkbench

## Принятые ограничения

- PhpStorm — ~15 лет, миллионы строк. Полный паритет нереалистичен; цель — 80%
  ежедневного workflow.
- Feature parity допустим, копирование кода/ассетов/бренда — нет.
- Zed выпускает релиз почти каждую неделю (1.25.3 — 23.10.2026). Upstream —
  движущаяся зависимость, а не база «один раз форкнули».
- Вся PHP-логика — в sidecar-процессах (LSP/DAP/JSON-RPC), не в крейтах ядра.

## Команда и бюджет

- 2–3 Rust-разработчика + дизайнер 0.5 FTE.
- 9–15 месяцев до «80% ежедневного PhpStorm».
- 150–300 человеко-месяцев до широкого паритета.
- Память: цель 1.5–2.5 ГБ в типичной сессии.

## Фаза 0 — ванильный Zed (неделя 1, 0 строк кода)

Поставляем `profile/`: settings, keymap, tasks, debug.

**Выход:** на реальном Laravel/Symfony-проекте (≥2000 файлов) работают:
навигация по `vendor/`, автодополнение, rename в 3+ файлах, брейкпоинт в
HTTP-запросе через Xdebug, запуск PHPUnit задачей.

**Гейт:** p95 автодополнения < 300 мс; ни одного падения LSP за 30 минут;
память процесса Zed + LSP < 1.5 ГБ.

**Риск:** нет PHP/Composer в системе — обязательный prereq
(`brew install php composer`) и `xdebug.mode=debug`.

## Фаза 1 — форк, де-брендинг, дисциплина upstream (недели 2–6)

- Клон стабильного тега в `vendor/zed`, своя ветка, `git rerere`.
- Патчи ядра только регистрационные: `crates/zed/src/zed.rs`
  (`initialize_panels`), `crates/workspace/src/dock.rs` (`Panel`),
  `crates/workspace/src/workspace.rs` (`add_panel`), схемы settings/keymap,
  ассеты.
- Удаление collab, telemetry, auto-update, AI-панелей — это **decoupling**,
  а не удаление крейтов: `editor`, `project`, `workspace` зависят от
  `client`/`rpc`/`telemetry`.
- CI: сборка форка против каждого upstream-тега, сетевой egress-аудит.

**Гейт:** macOS arm64 + Linux CI зелёные; приложение под своим bundle id;
egress без обращений к Zed/Sentry; одна кастомная панель открывается и
переживает рестарт; DMG < 120 МБ.

**Бюджет:** новые крейты 20–30k LOC, патчи ядра 2–5k LOC. Выше 10k LOC в ядре
еженедельные ребейзы становятся постоянным налогом.

## Фаза 2 — Search Everywhere и навигация (месяцы 2–3)

Единый поиск по файлам, символам, действиям, командам, composer-пакетам,
недавним. Ранжирование, превью, группы результатов.

**Гейт:** p95 < 150 мс на 50k файлов; eval-набор из 30 запросов, топ-1 ≥ 80%;
call/type hierarchy и «перейти к тесту» работают.

## Фаза 3 — панели инструментов (месяцы 3–5)

Статус: движки готовы — `tools/php-db` (SQLite, конфиг, schema/query, guard)
и `tools/php-http` (`.http`, переменные, env, JSON), `tools/deps-audit`.
UI-панелей к ним пока нет: это следующий шаг.

- **БД:** подключения (MySQL/PostgreSQL/SQLite), дерево схемы, редактор данных,
  консоль, completion по схеме. Крейт `php_database`, `sqlx`.
- **HTTP-клиент:** `.http`-файлы, окружения, ассерты, импорт cURL/OpenAPI.
- **Composer:** панель пакетов, add/remove, outdated, скрипты.
- **Тесты:** gutter-run PHPUnit/Pest через Zed runnables и `tags`, вывод и
  coverage.
- **Run configurations:** per-project, вместо ручных tasks.

**Гейт:** запрос к MySQL и PostgreSQL с результатом в гриде; `.http` с
переменными окружения и ассертом; установка пакета через панель с диффом
`composer.lock`; запуск теста из gutter с зелёным/красным статусом.

## Фаза 4 — рефакторинги и инспекции (месяцы 5–9)

- Extract method/variable/const, move class, inline, change signature —
  реализуются в LSP-слое (PHPantom/phpactor + собственные code actions),
  не в редакторе.
- Инспекции: overlay PHPStan/Psalm, debounce, severity-профили, quick fixes.
- Coverage, миграционные инспекции PHP-версий.

**Гейт:** набор acceptance-тестов на каждый рефакторинг (вход → ожидаемый
дифф); rename/extract не ломают `vendor`; PHPStan-диагностика появляется
< 3 с после сохранения на среднем проекте.

## Фаза 5 — remote, deploy, profiler (месяцы 9–15)

Статус: движок деплоя готов (`tools/php-deploy`: local/sftp, sync/upload/watch,
dry-run, исключения). Нет UI: панелей Remote/DB, окна настроек и интеграции с
событием сохранения. См. `docs/DEPLOY.md`.

SFTP/FTP-синхронизация, Docker/WSL-интерпретаторы, SSH-терминал,
Xdebug-профайлер со снапшотами.

**Гейт:** upload-on-save в staging; отладка внутри Docker с path mappings;
профиль запроса открывается как call tree.

## Фаза 6 — собственный движок типов (месяцы 15+)

Не писать парсер с нуля: форк PHPantom (MIT) или встраивание крейтов Mago
(syntax/HIR/names/semantics) за тонким `tower-lsp`. Нужны: Salsa-подобная
инкрементальная БД запросов, индекс Composer/PSR-4/classmap с ленивым
`vendor`, phpstorm-stubs, индекс ссылок, типы union/intersection, generics,
shape/conditional/aliases, flow narrowing.

**Гейт:** php-typing-conformance + корпуса фикстур PHPStan/Psalm;
регрессий нет на эталонных проектах.

## Постоянные практики

- Finding ledger: каждое решение — с источником (файл, URL, версия).
- ADR на каждое необратимое решение.
- Ребейз на стабильный тег раз в месяц, security-фиксы — еженедельно.
- Никаких «архитектур на будущее»: каждая фаза должна быть полезна сама по себе.
