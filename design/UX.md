# UX-спецификация phpworkbench

Цель — ежедневный workflow PhpStorm, а не визуальная копия. Ассеты JetBrains
не используются; тема `PhpStorm Dark` — только цвета Darcula.

## Целевой dock-лейаут

```
┌───────────────┬──────────────────────────────┬──────────────┐
│ LEFT          │ CENTER                       │ RIGHT        │
│ Project       │ Editor + tabs + breadcrumbs  │ PHP          │
│ Outline       │                              │ Remote*      │
│ Git           │                              │ Database*    │
├───────────────┴──────────────────────────────┴──────────────┤
│ BOTTOM: Terminal · Debugger · Problems                       │
└──────────────────────────────────────────────────────────────┘
* будущее
```

- **Left** — локальная работа: Project, Outline, Git (все три в одном доке,
  вкладками; одновременно видна одна).
- **Right** — контекст проекта: сейчас `php_panel`; позже Remote и Database.
- **Bottom** — вывод и исполнение: Terminal и Debugger (по умолчанию свернуты).

## Правила поведения панелей

1. **Приоритет активации:** Project > Outline > Git > PHP > Remote > Database.
   Новая панель не отбирает фокус у редактора, если не запрошена явно.
2. **Start-open:** Project, Terminal — открыты при старте; Outline, Git,
   Debugger, PHP — закрыты (по кнопке/хоткею). Right-док не занимает место,
   пока пуст.
3. **Tooltips:** у каждой кнопки — короткое имя панели (например «PHP»);
   подсказка не перекрывает текст редактора и не дублирует хоткей в заголовке.
4. **Ширина/высота:** Left 280 px, Right 320 px, Bottom 260 px; значения
   запоминаются и восстанавливаются при перезапуске.
5. **Один док — одна активная вкладка;** закрытие последней вкладки скрывает
   док, не ломая лейаут.

## Паритет с PhpStorm

| Фича | Статус | Где |
|---|---|---|
| Search Everywhere | частично | база Zed + LSP, своя панель — фаза 2 |
| Alt+Enter (quick fixes) | есть | LSP code actions |
| Gutter test run | частично | runnables/tasks, фаза 3 |
| DB console | нет | `php_database`, фаза 3 |
| `.http` client | нет | `php_http`, фаза 3 |
| Remote browser | нет | `php_deploy`, фаза 5 |
| Deployment settings window | частично | `deploy.json` + CLI, GUI — фаза 5 |

## Принципы дизайна

- **Без ассетов JetBrains:** свои имена, иконки, тема; цвета Darcula
  воспроизводятся по значениям, не копированием файлов.
- **Читаемый контраст:** текст `#A9B7C6` на `#2B2B2B` (≈7.5:1); вторичный
  текст не ниже 4.5:1; статусы отличаются не только цветом.
- **Дефолты ширины/высоты** фиксированы (см. выше) и не «прыгают» при
  открытии вкладок.
- **Keyboard-first:** любое действие доступно с клавиатуры; мышь — опция.
  Фокус всегда видим, `Esc` возвращает в редактор.

## Сегодня vs план

**Есть:** Zed-ядро (редактор, LSP/DAP, терминал, git), JetBrains keymap,
Project/Outline/Git слева, Terminal/Debugger снизу, панель `php_panel` справа,
тема `PhpStorm Dark`, PHP-интеллект через `phpantom_lsp`, Xdebug, деплой-CLI.

**План:** Right-док для Remote и Database; своя Search Everywhere; gutter-запуск
тестов; DB console; `.http`; Remote browser; окно настроек деплоя вместо
`deploy.json`. Фазы — `docs/PLAN.md`, матрица — `docs/FEATURES.md`.
