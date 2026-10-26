# php-db

A small database engine for a PhpStorm-like IDE, built as a standalone Rust CLI.
It reads connections from `.zed/databases.json` and shells out to platform CLIs
instead of linking database drivers.

## Build

```sh
cargo build            # target/debug/php-db
cargo test
cargo build --release  # target/release/php-db
```

Only `anyhow`, `serde`, and `serde_json` are direct dependencies.

## Configuration

`php-db` looks for `.zed/databases.json` in the current directory, then in each
parent directory. Use `--config PATH` to point at a specific file.

```json
{
  "connections": [
    { "name": "app", "driver": "sqlite", "path": "storage/app.sqlite" },
    {
      "name": "pg",
      "driver": "postgres",
      "url": "postgres://user@host:5432/db",
      "password_env": "PGPASSWORD"
    }
  ]
}
```

Relative SQLite paths resolve against the project root (the parent of `.zed`).
Passwords are never stored in the config: `password_env` names an environment
variable that is read at runtime. An unknown field such as `"password"` is
rejected, and a password embedded in a URL is redacted in output and flagged
with a warning.

## Commands

```sh
php-db list [--json]
php-db schema <connection> [--json]
php-db query <connection> "<sql>" [--json] [--allow-write]
```

Global option: `--config PATH`. Help and version: `-h`, `-V`.

- `list` never connects to PostgreSQL or MySQL. SQLite connections report
  `file exists` / `file missing`; server drivers report whether their client CLI
  is on `PATH`.
- `schema` lists tables and columns. SQLite uses `sqlite_master` plus
  `PRAGMA table_info`; PostgreSQL and MySQL use `information_schema`. Missing
  client CLIs fail with a clear message.
- `query` prints an aligned table (or JSON) and refuses `drop`, `truncate`, and
  `delete` unless `--allow-write` is passed. The guard skips string literals,
  quoted identifiers, and comments.

Exit codes: `0` success, `1` error, `2` destructive statement refused.

Driver requirements: `sqlite3` (SQLite), `psql` (PostgreSQL), `mysql` (MySQL).
PostgreSQL runs `psql <url> -X --csv ...`; MySQL parses the URL into flags and
passes the password through `MYSQL_PWD`; SQLite runs `sqlite3 -json <db> "<sql>"`.

## Fixture

`fixtures/db-test` contains `demo.sqlite` (a `users` table with 3 rows) and
`.zed/databases.json` with an `app` SQLite connection plus a placeholder `pg`
connection:

```sh
cd fixtures/db-test
../../tools/php-db/target/debug/php-db list
../../tools/php-db/target/debug/php-db query app "select id, email from users order by id" --json
```

## Limitations

- Each invocation is a single CLI call: no sessions, transactions across calls,
  pooling, or cancellation. SQLite never creates a missing database file.
- SQLite passes the whole string as one statement; multiple statements can
  produce concatenated JSON and are not supported.
- SQLite duplicate column names collapse in JSON mode, and `INTEGER PRIMARY KEY`
  reports `not_null: false` because `PRAGMA table_info` reports the raw flag.
- PostgreSQL and MySQL results are strings (or SQL `NULL`); column type metadata
  is not inferred. Nested JSON types are native only for SQLite.
- MySQL runs with `--raw`, so embedded tabs/newlines are not escaped, and a
  literal `"NULL"` is indistinguishable from SQL `NULL`.
- PostgreSQL command tags for non-`SELECT` statements are detected heuristically.
- The guard does not understand PostgreSQL dollar-quoted bodies; a keyword inside
  one would be refused. Only `drop`, `truncate`, and `delete` are guarded.
- `psql` and `mysql` support is implemented but was not exercised on this
  machine because neither client is installed.
