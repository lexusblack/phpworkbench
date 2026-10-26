# SPIKE — форк Zed, 2026-10-26

## Цель

Проверить на этой машине, что путь «форк Zed + собственный панельный крейт»
собирается, регистрируется и запускается.

## Окружение

- macOS, Apple M4 Pro, 48 ГБ RAM.
- Xcode 27.0 (27A266a), Metal Toolchain 27A266a (839 МБ, докачан).
- rustc/cargo 1.99.1.
- Homebrew 7.0.9 в `/opt/homebrew`, в PATH отсутствует.
- Исходники Zed: тег `v1.25.3`, shallow clone, 121 МБ.
- PHP 8.5.7 в `/opt/homebrew/bin`, в PATH отсутствует; Xdebug и Composer нет.

## Что сделано

Новый крейт `vendor/zed/crates/php_panel`:

- `Panel` + `Focusable` + `EventEmitter<PanelEvent>` + `Render`;
- правый док, ширина 280, иконка `FileCode`, action `php_panel::ToggleFocus`;
- `init()` регистрирует action на каждом workspace.

Патчи ядра — 4 файла, +21 строка:

| Файл | Изменение |
|---|---|
| `Cargo.toml` | member + workspace dependency |
| `crates/zed/Cargo.toml` | зависимость |
| `crates/zed/src/zed.rs` | import, `PhpPanel::load`, `add_panel_when_ready`, `php_panel::init` |
| `Cargo.lock` | обновлён cargo |

## Проверки

| Шаг | Результат |
|---|---|
| `cargo check -p php_panel` | ok, 1.44 с (warm) |
| `cargo check -p zed` | ok, 49.9 с (warm) |
| `cargo build -p zed` | ok, 3 м 53 с |
| бинарник | `target/debug/zed`, 1.1 ГБ |
| smoke launch, 10 с | процесс жив, ошибок в логе нет |
| диск | `target` 30 ГБ; `incremental` 13 ГБ удалён |

## Проблемы и решения

1. `cmake` не в PATH → использовать `/opt/homebrew/bin`.
2. В Xcode 27 нет Metal Toolchain →
   `xcodebuild -downloadComponent MetalToolchain` (839 МБ).
3. Debug-сборка занимает 30 ГБ → удалить `target/debug/incremental` (13 ГБ)
   или `cargo clean`.
4. `cx.new` из `AsyncWindowContext` требует `use gpui::AppContext`.

## Что не проверено

- PHP-стек: нет Composer и Xdebug на момент первого спайка.
- Де-брендинг, удаление collab/telemetry, CI — не начаты.

## Подтверждено вручную

- Панель `php_panel` видна в доке: tooltip ровно «PHP» (проверено
  пользователем 2026-10-26).
- Замер памяти: Zed 373 МБ + PHPantom 41 МБ ≈ 415 МБ.

## Следующие шаги

1. `brew install composer`; Xdebug под PHP 8.5 (pecl или сборка).
2. `cargo install --git https://github.com/PHPantom-dev/phpantom_lsp phpantom_lsp`.
3. Запустить форк на реальном PHP-проекте, снять скриншот панели.
4. Зафиксировать патч в `patches/series`, настроить ребейз и CI.

## Интеграция PHP — проверено 2026-10-26 19:01

- PHPantom 0.11.1 установлен (`cargo install --git ...`), бинарник
  `~/.cargo/bin/phpantom_lsp`, 51 МБ.
- PHP-расширение собрано из `zed-extensions/php` штатным `zed-extension`
  (wasi-sdk скачан в `.cache/wasi-sdk`).
- Расширение установлено в изолированный профиль
  `/tmp/zed-test/extensions/installed/php`.
- Доверие проекту внесено в `db/0-stable/db.sqlite`, таблица
  `trusted_worktrees` — без этого Zed не запускает LSP.
- Фикстура: `fixtures/demo` (PSR-4, `Greeter` / `GreetingService`).

**Результат:**

- Zed сам запустил `phpantom_lsp` (PID 24082, 41 МБ).
- Лог: `[lsp] starting language server process ... /Users/admin/.cargo/bin/phpantom_lsp`.
- Статус-бар: `PHP`; breadcrumbs `class GreetingService > $greeters`;
  outline: `class GreetingService`, `$greeters`, `function add`,
  `function greetAll`.
- Память: Zed 373 МБ + PHPantom 41 МБ ≈ 415 МБ.

**Грабли:**

- Установленному расширению нужен **сериализованный** манифест от сборщика,
  а не исходный `extension.toml`. В исходном нет `[lib]`, `languages`, а у
  грамматики php нет `path = "php"` — без этого PHP-язык не регистрируется.
  Рабочий манифест: `extensions/php-installed/extension.toml`.
- `zed-extension` зависает после записи артефактов (дедлок в `RealFs::drop`),
  но артефакты валидны: скрипт забирает их и убивает процесс.
- Для single-file worktree тоже нужен trust, иначе LSP не стартует.
- Лог: `~/Library/Logs/Zed/Zed.log`; `RUST_LOG=debug` работает.

**Воспроизведение:**

```bash
./scripts/install-php-extension.sh /tmp/zed-test
export PATH="/opt/homebrew/bin:$PATH"
cd vendor/zed && ./target/debug/zed --user-data-dir /tmp/zed-test ../../fixtures/demo
```

**Не проверено:** hover/completion через клавиатуру, Composer и Xdebug.
Кнопка `php_panel` подтверждена: tooltip «PHP».
