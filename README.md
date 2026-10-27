# phpworkbench

PhpStorm-класс рабочего процесса на базе Zed.

Это **не** точная копия PhpStorm и не будет ею: код, ассеты, иконки и EULA JetBrains
закрыты, а объём оригинала — миллионы строк и 15+ лет разработки. Цель другая:
повторить **ежедневный workflow** — навигация по PHP, рефакторинги, Xdebug,
тесты, БД, `.http`, Composer, деплой — с бюджетом памяти 1.5–2.5 ГБ
(«20 ГБ как в оригинале» сознательно не является целью).

## Архитектура в двух словах

1. **Zed** (GPL-3.0) — редактор, GPUI, LSP/DAP-клиенты, панели, терминал, git.
2. **PHP-интеллект** — отдельные процессы: LSP (PHPantom / phpactor /
   Intelephense), batch-анализаторы (PHPStan / Psalm), тулчейны (composer,
   phpunit, sqlx, `.http`).

Интеллект живёт **вне** форка. Это даёт минимум конфликтов с upstream,
переиспользование в VS Code / Neovim и чистую границу для проприетарных частей.

## Что можно включить уже сегодня

`profile/` — конфиг для ванильного Zed: PHP-LSP, PhpStorm-подобный keymap,
задачи Composer/PHPUnit/PHPStan, конфиг Xdebug. Кода форка не требует.

## Документы

| Файл | Назначение |
|---|---|
| `docs/PLAN.md` | фазы, оценки, критерии выхода |
| `docs/ARCHITECTURE.md` | слои, крейты, точки патча ядра |
| `docs/DECISIONS.md` | ADR: что выбрано и почему |
| `docs/FEATURES.md` | матрица паритета с PhpStorm |
| `docs/SPIKE.md` | результаты первого спайка форка |
| `patches/README.md` | карта патчей ядра Zed |

## Быстрый старт (без форка)

```bash
mkdir -p YOUR_PROJECT/.zed ~/.config/zed
cp profile/settings.json profile/tasks.json profile/debug.json YOUR_PROJECT/.zed/
cp ~/.config/zed/keymap.json ~/.config/zed/keymap.json.bak 2>/dev/null || true
cp profile/keymap.json ~/.config/zed/keymap.json
```

`settings/tasks/debug` — проектные, `keymap` — глобальный (Zed не читает keymap
из проекта). Старый keymap сохраняется в `.bak`.

> Тема `PhpStorm Dark` должна лежать в глобальном каталоге Zed:
> `mkdir -p ~/.config/zed/themes && cp profile/themes/phpstorm-dark.json ~/.config/zed/themes/`.
> Без этого `"theme": "PhpStorm Dark"` не загрузится и Zed откатится на дефолтную.

Установи расширение PHP в Zed (`zed: extensions` → PHP) и один из серверов:

```bash
brew install php composer
cargo install --git https://github.com/PHPantom-dev/phpantom_lsp phpantom_lsp
```

Проверка: открыть проект, Cmd+Shift+O (структура), Cmd+B (переход),
Alt+Enter (быстрые фиксы), F5 (Xdebug, нужен `xdebug.mode=debug`).

## Быстрый старт (форк)

```bash
./scripts/bootstrap-zed.sh
cd vendor/zed && cargo run -p zed
```

Готовый бандл `dist/PhpWorkbench.app` запускается с отдельным каталогом данных:

```bash
open -n dist/PhpWorkbench.app --args --user-data-dir ~/.phpworkbench
# или
./scripts/run-app.sh            # ZED_USER_DATA_DIR=... для своего пути
```

> Бандл собран в канале `stable`. Если рядом уже запущен другой Zed stable
> (даже из `target/debug`), сработает single-instance lock на порту 44438 и
> новый процесс завершится с `zed is already running`. Для параллельного
> запуска задайте канал: `ZED_RELEASE_CHANNEL=dev open -n ...` (переменная
> наследуется через `open`).

## Статус

- Фаза 0 (конфиг) — готова. Zed уже содержит `base_keymap: JetBrains`, он включён в
  `profile/settings.json`; `keymap.json` добавляет только недостающие биндинги.
- Спайк фазы 1 — выполнен: форк `v1.25.3` собирается, крейт `php_panel`
  компилируется и регистрируется, приложение стартует. Детали и ограничения —
  `docs/SPIKE.md`.
- PHP-интеллект проверен вживую: Zed сам поднимает `phpantom_lsp`, статус-бар
  показывает `PHP`, outline и breadcrumbs приходят от LSP. Установка
  расширения — `scripts/install-php-extension.sh`.
- Собственная панель `php_panel` видна в доке (tooltip «PHP») и показывает
  список PHP-файлов проекта с открытием по клику.
- PHP-тулчейн: PHP 8.5.12, Composer 2.10.3, Xdebug 3.5.3, PHPUnit 13.4.3
  (2 теста проходят).
- Аудит зависимостей: `deps-audit` (OSV + Packagist/npm) и LSP-расширение
  `deps-guard` — уязвимые пакеты подчёркиваются в `composer.json`/`package.json`.
- Деплой: `php-deploy` — SFTP/local, `sync`, `upload`, `watch`
  (авто-загрузка изменённых файлов). Подробности — `docs/DEPLOY.md`.
- БД: `php-db` — SQLite (schema/query/JSON, guard от записи), Postgres/MySQL
  через CLI. HTTP: `php-http` — `.http`-файлы, переменные, env, `--json`.
- Тема `PhpStorm Dark` (Darcula-палитра) — `profile/themes/`; UX-спека —
  `design/UX.md`.
- Search Everywhere в правой панели: фильтр по файлам, Remote, БД и HTTP,
  Enter открывает первый файл, Esc очищает.
- Фазы 2–6 — `docs/PLAN.md`.

## Лицензия

Собственные файлы этого репозитория — MIT. Форк Zed наследует GPL-3.0-or-later:
распространение бинарника требует публикации полного исходного кода.
Имя, иконки и тема — собственные, без марки Zed и JetBrains.


## Диск и бэкапы

`vendor` — симлинк на `/Volumes/QUICK/vendor` (исходники и `target/` на внешнем
диске). После каждого изменения форка:

```bash
./scripts/backup-vendor.sh
```

Bundle и копия `php_panel` кладутся в `backups/` и `recovery/` на внутреннем
диске. История потери и восстановления — `docs/RECOVERY.md`.
