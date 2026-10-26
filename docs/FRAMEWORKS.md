# Фреймворки: что работает, что ставится, чего нет

Проверено по исходникам Zed 1.25.3 и официальным расширениям (октябрь 2026).

## Матрица

| Стек | Синтаксис | Интеллект | Что нужно | Реалистичность |
|---|---|---|---|---|
| PHP (baseline) | расширение `php` | PHPantom / phpactor / Intelephense | уже стоит PHPantom | работает |
| Laravel (PHP-код) | то же | PHPantom заявляет Eloquent, фасады, `app()`, Livewire | ничего | работает частично |
| Laravel (LSP) | — | `laravel-official` (`laravel-lsp`) | расширение, статический бинарь | 1 час |
| Blade | расширение `blade` (tree-sitter-blade) | `laravel-official` / PHPantom | расширение + LSP | 1 час |
| Yii2 (код) | обычный PHP | только общий PHP-анализ | — | работает как PHP |
| Yii2 (фреймворк) | — | **нет ни у кого** | свой анализатор | 3–8 недель |
| React JSX/TSX | **встроено** (tsx-грамматика) | `vtsls` | Node | работает |
| jQuery | встроенный JS | только типы | `@types/jquery` + jsconfig | 30 мин |
| Vue 3 | расширение `vue` | Volar (`@vue/language-server`) | расширение + Node | 15 мин онлайн |

Ключевое: Zed из коробки содержит только bash/c/cpp/css/diff/env/go/json/markdown/
python/rust/typescript/tsx/yaml. PHP, Blade и Vue — всегда расширения.

Проверено вживую: `fixtures/frameworks/App.tsx` — статус-бар `TSX`, JSX
подсвечен, Zed запустил `vtsls`. Девять диагностик в фикстуре ожидаемы: рядом
нет `node_modules` с типами React. Для чистого результата нужен
`package.json` + `npm install`.

## Установка (когда появится UI/реестр)

1. `php` — уже установлено вручную (см. `scripts/install-php-extension.sh`).
2. `blade` — из реестра Zed; даёт язык `Blade` для `*.blade.php`.
3. `laravel-official` — LSP по маршрутам, view, config, env, Eloquent.
   Не ставить одновременно с community-версией `laravel`.
4. `vue` — грамматика + Volar; Zed сам ставит `@vue/language-server` и
   `typescript@6.0.3` через свой Node.
5. React/TSX — ничего не нужно, `vtsls` подтягивается автоматически.
6. jQuery — добавить `@types/jquery` в `devDependencies` и `jsconfig.json`.

## Чего не будет без своего анализатора

- Yii2: `Yii::$app`, `Widget::widget([...])`, атрибуты ActiveRecord из схемы БД,
  behaviors, `Yii::t()`, URL-правила.
- Laravel: динамические маршруты, runtime-контейнер, глубокий вывод пропсов
  Blade-компонентов.
- Vue/Nuxt: автоимпорты без сгенерированных деклараций.

## Аудит зависимостей

`tools/deps-audit` — свой бинарь (MIT), не зависит от фреймворка:

- читает `composer.lock` / `composer.json`, `package-lock.json` / `package.json`;
- спрашивает OSV (`api.osv.dev`) по ecosystem `Packagist` и `npm`;
- показывает установленную и последнюю версию, устаревание, CVE/GHSA;
- `--json` для машинной обработки; exit code 1 при найденных адвизори.

Запуск в Zed: палитра команд → `task: spawn` → `deps: audit`.

Реализованная подсветка: `deps-audit --lsp` публикует `publishDiagnostics`
для `composer.json` и `package.json`. Расширение `extensions/deps-guard`
объявляет сервер `deps_guard` для языка JSON, поэтому уязвимые зависимости
подчёркиваются прямо в файле, а счётчик диагностик виден в статус-баре.

Подключение:

```bash
cargo install --path tools/deps-audit
./scripts/install-deps-guard.sh /tmp/zed-test
```

В настройках проекта:

```json
{
  "languages": {
    "JSON": { "language_servers": ["json-language-server", "deps_guard"] }
  }
}
```

Ограничение: команда расширения использует абсолютный путь
`/Users/admin/.cargo/bin/deps-audit`. При переносе на другую машину
поправить `extensions/deps-guard/src/lib.rs` и пересобрать.

Проверено: в `fixtures/deps/composer.json` подчёркнуты laravel/framework,
symfony/http-foundation, guzzlehttp/guzzle и yiisoft/yii2; в статус-баре 4
диагностики; сервер запускается Zed как `/Users/admin/.cargo/bin/deps-audit --lsp`.
