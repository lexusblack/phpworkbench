# UI redesign: Remote Host и Database как в PhpStorm

## Проблема

Сейчас всё лежит в одной панели стопкой секций: файлы, Remote, БД, HTTP.
Это прототип, а не tool window. В PhpStorm это отдельные окна с тулбаром,
деревом, вкладками и диалогами.

## Целевой UX

### Общий каркас

- Одно окно инструментов справа, ширина по умолчанию 360 px.
- Вкладки сверху: `Files | Remote | Databases | HTTP | Collections`.
- У каждой вкладки свой тулбар и своя область контента на всю высоту.
- Активная вкладка запоминается в состоянии панели.
- Никакой горизонтальной прокрутки; длинные пути обрезаются с tooltip.

### Remote Host

```
[ Remote ]  [↻ Refresh] [⇅ Sync] [＋]
staging · sftp · deploy@host  ·  /var/www/app        (status)
remote / src / controllers                            (breadcrumb)
▾ src
  ▾ controllers
      UserController.php      12 KB
  index.php                    4 KB
▸ vendor
```

- Дерево: иконки папок/файлов, треугольник раскрытия, отступы по глубине.
- Одинарный клик по папке — раскрыть/свернуть; по файлу — выделить.
- Двойной клик по файлу — скачать и открыть в редакторе (вкладка с пометкой remote).
- Тулбар: Refresh (перечитать каталог), Sync (php-deploy sync), ＋ (форма сервера).
- Ошибки соединения — строкой статуса, не молча.

### Databases

```
[ Databases ] [🔌 Connect] [↻] [＋]
app · sqlite · demo.sqlite                            (status)
▾ app
  ▾ public
      users (3 columns)
        id      INTEGER  PK
        email   TEXT
      orders (5 columns)
────────────────────────────────────────────
Data: public.users                    [Rows: 3]
id | email               | created_at
1  | alice@example.com   | 2026-01-15
2  | bob@example.com     | 2026-02-20
```

- Слева дерево: connection → schema → table → columns.
- Снизу вкладка Data: таблица с заголовками и выровненными колонками (не `k=v`).
- Тулбар: Connect/Disconnect, Refresh, ＋ (форма подключения).
- SQL-консоль — следующая итерация (вкладка Query с вводом и результатом).

### Настройки подключений

- Модальный диалог (или полноэкранная форма в панели) с секциями:
  General (name, driver, host/port, user, database/path),
  Auth (password_env), SSH/SSL (опционально).
- Кнопки Test Connection / Save / Cancel.
- Никакого ручного редактирования JSON для типовых случаев.

## Критерии приёмки первой итерации

- Вкладки переключаются, каждая занимает всю высоту.
- Remote: тулбар, breadcrumb, дерево с иконками и раскрытием, двойной клик
  открывает скачанный файл.
- Databases: дерево connection → table → columns и нижняя таблица Data
  с заголовками и строками (выровненные колонки).
- Все прежние возможности (search, symbols, HTTP, collections) доступны
  в своих вкладках.
- Сборка `cargo check -p php_panel && cargo build -p zed` зелёная.

## Что не входит в первую итерацию

- SQL-консоль с вводом.
- Редактирование данных (inline edit, commit/rollback).
- SSL/SSH-туннели в UI.
- Контекстные меню и drag-and-drop.
- Множественные окна (detached tool windows).
