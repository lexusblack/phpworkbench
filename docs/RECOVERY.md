# Восстановление после потери vendor

## Что случилось

Внешний диск `/Volumes/QUICK` очищался из-за нехватки места (почти 50 ГБ
занимали сборочные артефакты). Каталог `/Volumes/QUICK/vendor` был удалён
вместе с клоном Zed, git-историей форка и `target/`.

Внешний репозиторий `/Users/admin/phpstorm` не пострадал: документация,
движки (`tools/`), расширения, фикстуры, профиль и скрипты на месте.

## Что восстановлено

1. Клон Zed `v1.25.3` заново в `/Volumes/QUICK/vendor/zed`
   (симлинк `/Users/admin/phpstorm/vendor -> /Volumes/QUICK/vendor`).
2. `crates/php_panel` восстановлен из базы сессий opencode:
   - полные чтения файла (5 чанков, строки 1–2944);
   - 23 последующих `edit`-операции агентов, применённые по порядку;
   - итог: 3467 строк, сохранён в `recovery/php_panel.rs`.
3. Интеграция в ядро: member/dependency в `Cargo.toml`, `php_panel` в
   `crates/zed/Cargo.toml`, загрузка и `init` в `crates/zed/src/zed.rs`.
4. Пересборка: `cargo build -p zed` (~9 мин), приложение запускается,
   панель и LSP работают.

## Как сохранять дальше

Vendor остаётся на внешнем диске (места там много), исходники бэкапятся
на внутренний:

```bash
./scripts/backup-vendor.sh
```

Скрипт создаёт git-bundle в `backups/` и копирует `php_panel` в `recovery/`.
`backups/` в git не коммитится.

Правила:
- после каждого коммита в форке запускать `scripts/backup-vendor.sh`;
- не хранить в vendor ничего, кроме исходников и `target/`;
- `target/debug/incremental` удалять после сборки (экономит ~10 ГБ);
- перед очисткой диска проверять, что bundle существует и не пуст.

## Проверка восстановления

- `git -C vendor/zed log --oneline` — ветка `phpworkbench`;
- `cargo build -p zed` — зелёная;
- запуск на `fixtures/workbench` — вкладки Files/Remote/Databases/HTTP/
  Collections, Remote-дерево, PHP LSP (phpantom + php-lint).


## Важные грабли

1. **«Move Zed to Applications?»** — dev-бинарь предлагает «установиться» и
   копирует весь `target/debug` (десятки ГБ) в `/Applications/debug`.
   В форке этот промпт отключён в `crates/zed/src/main.rs`
   (закомментирован `move_to_applications::init`). Не соглашайтесь на него.
2. **Зависание в `_dyld_start`** после пересборки: процесс с RSS ~32 КБ и без
   окна. Лечится повторной ad-hoc подписью:
   `codesign --force --sign - vendor/zed/target/debug/zed` и повторным запуском.
3. `target/debug/incremental` удалять после сборки — экономит ~10 ГБ.
