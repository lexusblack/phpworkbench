# Матрица паритета

Легенда: `да` — есть в Zed + LSP, `конфиг` — включается профилем, `частично` —
есть база, `форк` — нужен крейт/патч, `sidecar` — отдельный процесс, `нет` —
отсутствует. Приоритет: M — ежедневно, I — важно, R — редко/энтерпрайз.

## Редактор и UX

| Фича | Пр. | Zed | План | Фаза |
|---|---|---|---|---|
| Type-aware completion | M | да | LSP | 0 |
| Live templates / postfix | M | частично | сниппеты + LSP | 0 |
| Multi-caret, column select | I | да | — | — |
| Inlay / parameter hints | I | да | настройки | 0 |
| Breadcrumbs | I | нет | форк (editor) | 2 |
| PSR-12 / EditorConfig reformat | M | частично | внешний форматтер | 1 |
| Local History | I | нет | форк (snapshot-крейт) | 4 |
| TODO tool window | I | частично | поиск + панель | 2 |
| Scratch files | I | частично | форк (buffer) | 2 |

## Навигация

| Фича | Пр. | Zed | План | Фаза |
|---|---|---|---|---|
| Search Everywhere | M | частично | форк-панель | 2 |
| Go to declaration / implementation | M | да | LSP | 0 |
| Find Usages с превью | M | да | LSP | 0 |
| Call / type hierarchy | I | частично | LSP + панель | 2 |
| Переход к тесту | I | нет | форк | 3 |
| Recent files, bookmarks | M | частично | форк | 2 |
| Structure view | I | да | outline | 0 |
| Breadcrumb navigation | I | нет | форк | 2 |

## Code intelligence

| Фича | Пр. | Zed | План | Фаза |
|---|---|---|---|---|
| PHP type inference + PHPDoc | M | да (LSP) | PHPantom | 0 |
| Generics (PHPStan/Psalm-аннотации) | I | частично | PHPantom | 0 |
| Composer autoload / PSR-4 | M | да (LSP) | sidecar-индекс | 0 |
| Laravel / Symfony / WordPress | M | частично | PHPantom + плагины | 4 |
| Blade / Twig / Smarty | I | частично | tree-sitter + LSP | 3 |
| PHP 8.4 (enums, hooks, aviz) | I | да | PHPantom | 0 |
| SQL/HTML/JS в строках | I | частично | tree-sitter injections | 3 |

## Рефакторинги

| Фича | Пр. | Zed | План | Фаза |
|---|---|---|---|---|
| Safe rename | M | да | LSP | 0 |
| Extract method/variable/const | M | частично | LSP code actions | 4 |
| Inline | I | частично | LSP | 4 |
| Move class/member/file | I | частично | phpactor RPC | 4 |
| Change signature | I | нет | свой code action | 4 |
| Pull up / push down / extract interface | R | нет | свой code action | 5 |
| Preview и undo рефакторинга | I | нет | форк (diff-панель) | 4 |

## Инспекции

| Фича | Пр. | Zed | План | Фаза |
|---|---|---|---|---|
| On-the-fly inspections | M | да (LSP) | PHPantom | 0 |
| Alt+Enter quick fixes | M | да | LSP | 0 |
| Undefined / unused symbols | M | да | PHPantom | 0 |
| PSR-12 / code style | I | частично | PHP-CS-Fixer overlay | 4 |
| Миграции версий PHP | I | нет | overlay | 4 |
| Security (SQLi, XSS) | I | нет | PHPStan/Psalm overlay | 4 |
| PHPStan / Psalm / PHPCS / PHPMD | I | частично | sidecar | 4 |

## Отладка

| Фича | Пр. | Zed | План | Фаза |
|---|---|---|---|---|
| Xdebug zero-config | M | да | встроенный адаптер | 0 |
| Line / conditional / exception breakpoints | M | да | — | 0 |
| Stepping, inline values, frames | M | да | — | 0 |
| Variables, watches, evaluate | M | да | — | 0 |
| Remote debug + path mappings | I | да | debug.json | 0 |
| Debug CLI / tests / HTTP | M | частично | tasks + adapter | 1 |
| Docker / WSL / SSH debug | I | частично | форк (интерпретаторы) | 5 |

## Тесты

| Фича | Пр. | Zed | План | Фаза |
|---|---|---|---|---|
| PHPUnit gutter run/debug | M | частично | runnables + tags | 3 |
| Pest (parallel, mutation) | M | частично | задачи + парсер | 3 |
| Coverage | I | нет | Xdebug + JUnit | 4 |
| Auto-rerun failed / on change | I | нет | форк | 4 |
| Генерация тестов / data sets | I | нет | форк | 5 |
| Behat / Codeception / PHPSpec | R | нет | задачи | 5 |

## Базы данных

| Фича | Пр. | Zed | План | Фаза |
|---|---|---|---|---|
| MySQL / PostgreSQL / SQLite | M | нет | `php_database` | 3 |
| MongoDB | R | нет | sidecar | 5 |
| Дерево схемы, data editor | M | нет | форк-панель | 3 |
| SQL completion / resolve | M | нет | sidecar + LSP | 3 |
| Query console, explain | I | нет | форк-панель | 3 |
| Table/column refactorings | I | нет | sidecar | 4 |
| CSV/JSON import-export | I | нет | sidecar | 4 |

## HTTP-клиент

| Фича | Пр. | Zed | План | Фаза |
|---|---|---|---|---|
| `.http` в редакторе | M | нет | `php_http` | 3 |
| Окружения и приватные переменные | I | нет | `php_http` | 3 |
| Ассерты по ответу | I | нет | `php_http` | 3 |
| Импорт cURL / Postman / OpenAPI | I | нет | `php_http` | 4 |
| WebSocket / GraphQL / HTTP2 | I | нет | `php_http` | 4 |
| Debug HTTP через Xdebug | I | нет | интеграция с DAP | 4 |

## Composer

| Фича | Пр. | Zed | План | Фаза |
|---|---|---|---|---|
| `composer.json` completion/validation | M | частично | JSON-LSP + схема | 0 |
| Add/update из UI | I | нет | `php_ide_panel` | 3 |
| Запуск команд и скриптов | M | конфиг | tasks | 0 |
| PSR-4 / vendor resolve | M | да | LSP | 0 |
| Outdated / dependency check | I | конфиг | task | 0 |

## Деплой и remote

| Фича | Пр. | Zed | План | Фаза |
|---|---|---|---|---|
| SFTP / FTP / FTPS / WebDAV | I | нет | `php_deploy` | 5 |
| Upload on save, diff, sync | I | нет | `php_deploy` | 5 |
| Remote file browser | I | нет | `php_deploy` | 5 |
| SSH terminal | I | нет | терминал + russh | 5 |
| Docker / Compose интерпретаторы | M | нет | форк | 5 |
| WSL | I | да (Windows) | — | — |
| Remote interpreters + mappings | I | нет | форк | 5 |
| Gateway remote dev | R | частично | remote-сервер | 6 |

## VCS, терминал, прочее

| Фича | Пр. | Zed | План | Фаза |
|---|---|---|---|---|
| Commit / log / annotate | M | да | — | 0 |
| Branch / merge / rebase / conflicts | M | да | — | 0 |
| Stash / shelve / changelists | I | частично | форк | 4 |
| GitHub / GitLab PR | I | частично | расширение/CLI | 4 |
| SVN / Mercurial / Perforce | R | нет | — | — |
| Integrated terminal, splits | M | да | — | 0 |
| Shell integration, clickable paths | M | да | — | 0 |
| Docker / WSL / SSH terminals | I | частично | форк | 5 |
| Xdebug profiler | I | нет | sidecar + панель | 5 |
| Plugin marketplace | M | нет | форк (WASM + UI API) | 6 |
| AI chat / edit / completion | I | да | ACP | 0 |
| Junie-подобный агент | I | да | ACP-агенты | 0 |

## 15 фич, которые создают «ощущение PhpStorm»

1. Search Everywhere.
2. Alt+Enter.
3. Безопасный rename/extract.
4. Type-aware completion.
5. Xdebug из коробки.
6. Gutter-запуск тестов.
7. Инструменты БД.
8. `.http`-клиент.
9. Remote interpreters.
10. Composer/фреймворк-awareness.
11. Стек инспекций (PHPStan/Psalm/PHPCS).
12. Git-аннотации и Local History.
13. Local History.
14. Встроенный терминал.
15. AI-ассистент/агент.
