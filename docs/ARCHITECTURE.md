# ARCHITECTURE

## Слои

```
┌──────────────────────────────────────────────────────────────┐
│ Zed fork (GPL-3.0)                                           │
│  editor · GPUI · workspace/dock · project · terminal · git   │
│  + php_ide, php_ide_panel, php_database, php_http, php_tests │
└───────┬───────────────┬──────────────────┬───────────────────┘
        │ LSP           │ DAP              │ JSON-RPC
        ▼               ▼                  ▼
┌───────────────┐ ┌─────────────┐ ┌────────────────────────────┐
│ PHPantom      │ │ Xdebug      │ │ sidecars:                  │
│ phpactor      │ │ adapter     │ │ composer · sqlx · .http    │
│ Intelephense  │ │ (9003)      │ │ phpunit · sftp · phpstan   │
│ (BYO licence) │ │             │ │ psalm (batch, on demand)   │
└───────────────┘ └─────────────┘ └────────────────────────────┘
```

Правило: **панель ничего не анализирует сама**. Она рисует данные, которые
отдаёт sidecar по документированному протоколу.

## Крейты форка

| Крейт | Ответственность | Зависимости |
|---|---|---|
| `php_ide` | регистрация LSP, настройки, команды | `language`, `project` |
| `php_ide_panel` | `Panel`-реализация: outline/Search Everywhere | `workspace`, `gpui` |
| `php_database` | подключения, схема, грид | `sqlx`, `gpui` |
| `php_http` | `.http`-парсер и раннер | `reqwest`, `gpui` |
| `php_tests` | runnables, парсинг JUnit/TeamCity | `task`, `gpui` |
| `php_deploy` | SFTP/SSH-синк | `russh`, `ssh2` |

Новые крейты — GPL, потому что линкуются с GPL-крейтами. Проприетарное
допустимо только как отдельный процесс с документированным протоколом.

## Точки патча ядра

| Файл | Что делаем |
|---|---|
| `crates/zed/src/zed.rs` | `initialize_panels`: добавляем панели |
| `crates/workspace/src/dock.rs` | `Panel` trait, `DockPosition` |
| `crates/workspace/src/workspace.rs` | `add_panel`, `toggle_panel_focus` |
| `crates/editor/` | gutter-раннеры, breadcrumbs (минимально) |
| `crates/settings/` | схемы новых настроек |
| `assets/keymaps/` | дефолтный PhpStorm-keymap |
| `crates/zed/Cargo.toml` | feature flags: `php`, `collab`, `telemetry`, `ai` |

Бюджет: 2–5k LOC. Всё, что можно вынести в крейт — выносим.

## Потоки данных

1. **Правка:** `didChange` → LSP → диагностики/подсказки. Никакого проектного
   индекса в редакторе.
2. **Search Everywhere:** fan-out по провайдерам (файлы, символы LSP, действия,
   команды, composer) → слияние и ранжирование в `php_ide_panel`.
3. **Тесты:** task с `tags: ["phpunit"]` → runnable в gutter → терминал →
   парсинг JUnit XML → статусы.
4. **БД:** панель → sidecar → `sqlx`; креды в macOS Keychain, не в settings.
5. **Batch-анализ:** PHPStan/Psalm запускается по требованию, результат
   кэшируется и подаётся как диагностики с debounce. В горячем пути его нет.

## Бюджет памяти

| Компонент | Типично |
|---|---|
| Zed UI + редактор | 250–600 МБ |
| LSP (PHPantom) | 300–700 МБ |
| Intelephense (если включён) | 700–900 МБ |
| PHPStan (batch, on demand) | 1–2 ГБ |
| Панели и sidecars | 50–150 МБ |
| **Итого в сессии** | **1.5–2.5 ГБ** |

Меры: `vendor/` не индексируется целиком, ленивый парсинг, mmap-кэш,
лимиты на batch-процессы, выгрузка неактивных панелей.

## Отказоустойчивость

- LSP упал → рестарт с backoff, статус в статус-баре, диагностики помечаются
  как устаревшие.
- Индекс холодный → прогресс-бар, навигация по файлу доступна сразу.
- Sidecar завис → таймаут и kill, панель показывает ошибку, редактор живёт.
- Оффлайн → никакой деградации: телеметрии и удалённых зависимостей нет.
