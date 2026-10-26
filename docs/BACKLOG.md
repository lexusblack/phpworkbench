# BACKLOG — что осталось до «готово»

Статусы: `[x]` готово и проверено, `[~]` частично, `[ ]` не начато.
Критерий готовности — проверяемый результат, а не «работает на глаз».

## Фаза 1. Форк, бренд, инфраструктура

- [~] Форк Zed, ветка `phpworkbench`, сборка на macOS
- [ ] Де-брендинг: имя, bundle id, иконка, About, CLI, домены
- [ ] Удаление collab/telemetry/auto-update (feature flags + egress-аудит)
- [ ] Release-бандл `.app` (debug-бинарь не упаковывается: ассеты из исходников)
- [ ] Подпись и нотаризация
- [ ] CI: сборка форка против upstream-тегов, тесты, линт
- [ ] Регламент ребейза (стабильные теги, `rerere`, patch queue)
- [ ] GPL-нотисы и атрибуция «based on Zed»

## Фаза 2. Search Everywhere и навигация

- [ ] Единый поиск: файлы + символы + действия + команды + composer-пакеты
- [ ] Ранжирование и превью
- [ ] Eval-набор 30 запросов, top-1 ≥ 80 %
- [ ] Call/type hierarchy
- [ ] Breadcrumbs в редакторе
- [ ] «Перейти к тесту»
- [ ] Recent files и bookmarks

## Фаза 3. Панели инструментов

- [~] Движок БД (`php-db`) — SQLite, schema/query/JSON, guard
- [~] Движок HTTP (`php-http`) — `.http`, переменные, env, JSON
- [~] Аудит зависимостей (`deps-audit`) + LSP-подсветка
- [~] Деплой (`php-deploy`) — local/sftp, sync/upload/watch
- [~] Панель Remote в правом доке: список серверов + вызов sync (клик не проверен автоматически)
- [~] Панель Databases: список подключений (нет схемы, грида и консоли)
- [~] Панель HTTP: список .http-файлов и открытие (нет запуска и ответа)
- [ ] Composer-панель (add/remove/outdated)
- [ ] Gutter-запуск PHPUnit/Pest + coverage
- [ ] Run configurations

## Фаза 4. Рефакторинги и инспекции

- [ ] Extract method/variable/const
- [ ] Inline
- [ ] Move class/member/file
- [ ] Change signature
- [ ] Pull up / push down / extract interface
- [ ] Превью и undo рефакторинга
- [ ] Overlay PHPStan/Psalm с debounce
- [ ] PSR-12 и миграционные инспекции
- [ ] Coverage
- [ ] Local History

## Фаза 5. Remote, deploy UI, profiler

- [~] Движок деплоя (local/sftp, watch)
- [ ] Remote-панель и дерево файлов на сервере
- [ ] Редактор удалённых файлов
- [ ] Окно настроек деплоя (GPUI-модал)
- [ ] Upload-on-save через событие `BufferEvent::Saved`
- [ ] Delete-propagation и разрешение конфликтов
- [ ] Docker/WSL/SSH-интерпретаторы
- [ ] Xdebug-профайлер и call tree

## Фаза 6. Свой движок типов

- [ ] Форк PHPantom/Mago за `tower-lsp`
- [ ] Salsa-подобный инкрементальный индекс
- [ ] Composer/PSR-4/classmap + ленивый vendor
- [ ] Generics, shapes, conditionals, flow narrowing
- [ ] phpstorm-stubs
- [ ] Соответствие php-typing-conformance и фикстурам PHPStan/Psalm

## Фреймворки

- [x] React/TSX (встроено, проверено)
- [~] Laravel: PHPantom частично; полноценно — расширение `laravel-official`
- [ ] Blade: расширение `blade`
- [ ] Vue 3: расширение `vue` + Volar
- [ ] jQuery: `@types/jquery`
- [ ] Yii2: фреймворк-интеллект отсутствует, нужен свой анализатор

## Качество и эксплуатация

- [x] PHPUnit-фикстура, 2 теста
- [x] deps-audit: 52 адвизори на фикстуре
- [x] php-db: 13 тестов
- [x] Acceptance-скрипт по всем движкам (`scripts/acceptance.sh`, 11 проверок)
- [ ] Тесты панелей и LSP-интеграции
- [ ] Замеры памяти и времени отклика на реальном проекте
- [ ] Документация установки «с нуля»

## Ближайшие 3 шага

1. Панель Workbench: файлы + Remote + Databases + HTTP (правый док).
2. Acceptance-скрипт и CI.
3. Release-бандл для ежедневного использования.
