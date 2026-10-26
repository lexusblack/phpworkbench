# php-db

A small, PhpStorm-like database connection layer for an IDE, built as a
standalone Rust CLI. It reads connections from `.zed/databases.json` and shells
out to the official database CLIs; no database driver crates are linked.

## Build

```sh
cargo build              # target/debug/php-db
cargo test
cargo clippy --all-targets
cargo build --release    # target/release/php-db
```

Only `anyhow`, `serde`, and `serde_json` are direct dependencies.

## Configuration

`php-db` looks for `.zed/databases.json` in the current directory, then in each
parent directory. Use `--config PATH` to point at a specific file. Relative
SQLite paths resolve against the project root (the parent of `.zed`).

```json
{
  "connections": [
    { "name": "app", "driver": "sqlite", "path": "demo.sqlite" },
    {
      "name": "pg",
      "driver": "postgres",
      "url": "postgres://user@host:5432/db",
      "password_env": "PGPASSWORD",
      "sslmode": "require",
      "ssh_tunnel": {
        "host": "bastion.example.com",
        "user": "deploy",
        "port": 22,
        "identity_file": "~/.ssh/id_ed25519"
      }
    },
    {
      "name": "my",
      "driver": "mysql",
      "host": "h",
      "port": 3306,
      "user": "u",
      "database": "d",
      "password_env": "MYSQL_PWD",
      "socket": "/tmp/mysql.sock",
      "ssl": true
    }
  ]
}
```

Known fields: `name`, `driver`, `path`, `url`, `host`, `port`, `user`,
`database`, `password_env`, `socket`, `sslmode`, `ssl`, and `ssh_tunnel`
(`host`, `user`, `port`, `identity_file`). The old `{name, driver, path, url}`
shape remains valid. Unknown drivers and unknown fields are rejected.

Passwords are never stored as a config field. `password_env` names an
environment variable read at runtime. A password embedded in `url` is allowed
for compatibility but produces a warning, is redacted from every rendered
target, and is stripped from client argv where the client supports an
environment variable. The `ssh_tunnel` block is modelled for future use but is
**not active yet**; `list` warns when one is configured.

## Drivers

| Driver | Client | Target / notes |
| --- | --- | --- |
| `sqlite` | `sqlite3` | `path`; full schema and SQL query support |
| `postgres` | `psql` | `url` or `host`/`user`/`database`; `PGPASSWORD`; `sslmode` |
| `mysql` | `mysql` | `url`, or `host`/`socket` plus `database`; `MYSQL_PWD`; `--ssl-mode=REQUIRED` when `ssl` |
| `mariadb` | `mysql` | Same connection model; `MYSQL_PWD`; `--ssl` when `ssl` |
| `sqlserver` | `sqlcmd` | `url` or `host`, plus `database`; `SQLCMDPASSWORD`; `-C` when `ssl` |
| `oracle` | `sqlplus` | `url` (`user@host:port/service`) or `host`/`user`; password via stdin `CONNECT`, never argv |
| `mongodb` | `mongosh` | `url` or `host`; credentials stripped from the URI, then `db.auth()` from env |
| `redis` | `redis-cli` | `url`, `host`, or `socket`; `REDISCLI_AUTH`; `--tls` for `rediss://` or `ssl` |

Each adapter exposes availability checking, target rendering, check/ping command
construction, a best-effort schema command, and a clear missing-client error.
Only SQL drivers support `query`.

## Commands

```sh
php-db list [--json]
php-db check <connection> [--json]
php-db schema <connection> [--json]
php-db query <connection> "<sql>" [--json] [--allow-write]
php-db exec <connection> "<command>" [--allow-write]
```

- `list` prints name, driver, redacted target, client found/missing, and status.
  It never opens a connection. `--json` includes `client_available`,
  `reachable`, and `warnings`.
- `check` actually pings: SQLite runs `SELECT 1`, server drivers run their
  client's one-line probe, MongoDB checks `{ping:1}`, and Redis checks `PONG`.
- `schema` is full for SQLite (tables, columns, PK/NOT NULL/default). For
  PostgreSQL, MySQL/MariaDB, SQL Server, and Oracle it lists tables. MongoDB
  lists collections; Redis lists keys via `--scan` (best-effort keyspace).
- `query` is SQL-only and prints an aligned table or `--json`. Destructive
  `drop`, `truncate`, and `delete` statements exit with code 2 unless
  `--allow-write` is supplied. The guard skips literals, quoted identifiers, and
  comments.
- `exec` runs a Redis argv command or a MongoDB `--eval` script. It is a
  write-capable escape hatch and always requires `--allow-write`.

Exit codes: `0` success, `1` error (including a missing client), `2` refused
statement or `exec` without `--allow-write`.

## Fixture

`fixtures/db-drivers` contains `demo.sqlite` (a `users` table with 3 rows) and a
`.zed/databases.json` with all eight drivers:

```sh
cd fixtures/db-drivers
../../tools/php-db/target/debug/php-db list
../../tools/php-db/target/debug/php-db check app
../../tools/php-db/target/debug/php-db schema app --json
../../tools/php-db/target/debug/php-db query app \
  "select id, email from users order by id" --json
```

## Limitations

- No real PostgreSQL, MySQL, MariaDB, SQL Server, Oracle, MongoDB, or Redis
  server was available while building this. Those paths are verified by unit
  tests for argv/environment construction and by deterministic missing-client
  error tests; only SQLite is exercised end-to-end.
- SSH tunnels are modelled but not opened. Connections that require one will
  fail at the client level until tunnel support is implemented.
- Non-SQLite `schema` is a table/collection/keyspace listing without column
  metadata. Redis `--scan` can be large on a populated server.
- Oracle output uses SQL*Plus `SET MARKUP CSV`; formatting is best-effort.
  The password is sent over stdin in a `CONNECT` statement.
- MongoDB connects without credentials and then calls `db.auth()` using
  environment variables; deployments that reject unauthenticated connections
  may need a URI-based auth flow.
- SQL Server results are parsed from tab-separated `sqlcmd` output; embedded
  tabs/newlines and empty columns are best-effort.
- `query` sends one statement per invocation. There are no sessions,
  transactions across calls, pooling, or cancellation.
- SQLite never creates a missing database file. Multiple SQLite statements can
  produce concatenated JSON, and duplicate column names collapse in JSON mode.
- The destructive guard understands string literals, quoted identifiers, and
  comments, but not PostgreSQL dollar-quoted bodies. Only `drop`, `truncate`,
  and `delete` are guarded.
- PostgreSQL and MySQL command tags are detected heuristically; non-`SELECT`
  statements may report no rows.
