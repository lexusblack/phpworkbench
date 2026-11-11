# Карта патчей ядра Zed

Правило: в ядро — только регистрация и точки расширения. Логика — в новых
крейтах. Бюджет: 2–5k LOC. Выше 10k LOC ребейзы становятся постоянным налогом.

## Обязательные точки

### 1. `crates/zed/src/zed.rs`

`initialize_panels()` загружает панели параллельно и вызывает
`workspace.add_panel(panel, window, cx)`, затем `finish_dock_restoration`.
Здесь регистрируем `php_ide_panel`, `php_database`, `php_http`.

Изменение: ~30 строк + feature flag `php`.

### 2. `crates/workspace/src/dock.rs`

`Panel` trait: `persistent_name`, `panel_key`, `position`, `position_is_valid`,
`set_position`, `default_size`, `icon`, `icon_tooltip`, `toggle_action`,
`starts_open`, `activation_priority`, `enabled`.
`Dock`, `DockPosition {Left, Bottom, Right}`, `PanelEvent`.

Изменение: обычно **не требуется**. Трогаем, только если нужна новая позиция
дока или поведение.

### 3. `crates/workspace/src/workspace.rs`

`Workspace::add_panel`, `toggle_panel_focus::<T>`.

Изменение: не требуется, если панель реализует `Panel`.

### 4. `crates/editor/`

Gutter-раннеры для тестов и breadcrumbs.

Изменение: 100–300 строк, аккуратно — самый конфликтный файл. Сначала пробуем
через runnables/tasks; патч — только если UX не достигается.

### 5. `crates/settings/`

Схемы: `php_ide`, `php_database`, `php_http`, `php_deploy`.

Изменение: по одному struct на крейт, ~50 строк каждый.

### 6. `assets/keymaps/` + `assets/icons/`

Дефолтный PhpStorm-keymap и иконки панелей. Только собственные ассеты.

### 7. `crates/zed/Cargo.toml`

Feature flags: `php` (вкл.), `collab`, `telemetry`, `ai`, `auto_update`.
Де-брендинг — через флаги + удаление зависимостей, а не удаление крейтов:
`editor`, `project`, `workspace` зависят от `client`/`rpc`/`telemetry`.

## Порядок работы

1. Форк от стабильного тега, ветка `phpworkbench`.
2. Каждый патч — отдельный коммит с префиксом `core:`.
3. Патч-очередь в `patches/series` (quilt-подобно), ребейз раз в месяц.
4. CI собирает форк против нового тега; красный CI блокирует ребейз.
5. `git rerere` включён.

### 8. `crates/zed/src/main.rs`, `crates/zed/src/zed/app_menus.rs`, `crates/zed/Cargo.toml`

Автообновление вырезано целиком: убраны вызовы `auto_update::init` /
`auto_update_ui::init`, пункты меню «Check for Updates» и «View Release Notes
Locally», зависимости `auto_update`/`auto_update_ui` у крейта `zed`. Причина:
стабильный канал форка опрашивал релизы upstream и подменял приложение
официальным Zed (воспроизведено 2026-10-30: форк 19:17:59 → официальный Zed
19:18:34). Дефолт `"auto_update": false` в `assets/settings/default.json`
оставлен как страховка. Крейт `auto_update` остаётся зависимостью
`title_bar`/`activity_indicator`/`remote_connection` — скачивание
remote-server для SSH работает.

## Локальные патчи инструментов

- `crates/extension_cli/src/main.rs` — печать ошибки до `std::process::exit`
  и вывод сериализованного манифеста (`MANIFEST_BEGIN`). Нужно, потому что CLI
  зависает в `RealFs::drop` и скрывает ошибку, а установленному расширению
  требуется именно сериализованный манифест.

## Чего не делаем

- Не переписываем `editor`/`project`/`workspace`.
- Не тащим `collab`-код «на всякий случай».
- Не добавляем UI-API для плагинов в фазе 1–4: только после стабилизации
  собственных панелей.
- Не патчим `gpui` без необходимости: пин на git-ревизию, 1–2 дня на апдейт.
