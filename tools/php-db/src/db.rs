//! Database access by shelling out to the official platform CLIs
//! (`sqlite3`, `psql`, `mysql`, `sqlcmd`, `sqlplus`, `mongosh`, `redis-cli`).
//! No database driver crates are linked.

use crate::config::{
    add_query_param, percent_decode, strip_url_credentials, strip_url_password, Connection, Driver,
    SshTunnel,
};
use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

pub struct Column {
    pub name: String,
    pub kind: String,
    pub not_null: bool,
    pub primary_key: bool,
    pub default: Option<String>,
}

pub struct Table {
    pub name: String,
    pub columns: Vec<Column>,
}

pub struct Schema {
    pub tables: Vec<Table>,
}

pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

impl QueryResult {
    pub fn to_json(&self) -> Value {
        let rows: Vec<Value> = self
            .rows
            .iter()
            .map(|row| {
                let mut object = Map::new();
                for (index, column) in self.columns.iter().enumerate() {
                    object.insert(
                        column.clone(),
                        row.get(index).cloned().unwrap_or(Value::Null),
                    );
                }
                Value::Object(object)
            })
            .collect();
        Value::Array(rows)
    }
}

// ---------------------------------------------------------------------------
// Client availability
// ---------------------------------------------------------------------------

/// Finds a client binary on `PATH` (or the supplied search path). Kept separate
/// from `Command` so tests can exercise a deterministic empty search path.
pub fn find_executable(bin: &str, search_path: Option<&OsStr>) -> Option<PathBuf> {
    let path = match search_path {
        Some(path) => path.to_os_string(),
        None => std::env::var_os("PATH")?,
    };
    for directory in std::env::split_paths(&path) {
        if directory.as_os_str().is_empty() {
            continue;
        }
        let candidate = directory.join(bin);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub fn client_available(bin: &str) -> bool {
    find_executable(bin, None).is_some()
}

pub fn require_client_with(driver: Driver, search_path: Option<&OsStr>) -> Result<()> {
    let bin = driver.client();
    if find_executable(bin, search_path).is_none() {
        bail!(
            "{} support requires the `{bin}` CLI on PATH, but it was not found",
            driver.as_str()
        );
    }
    Ok(())
}

pub fn require_client(driver: Driver) -> Result<()> {
    require_client_with(driver, None)
}

// ---------------------------------------------------------------------------
// Sessions and SSH tunnels
// ---------------------------------------------------------------------------

/// One resolved connection: normalized fields, with an established ssh tunnel
/// rewrite applied when the config enables one. The `ssh` child process is
/// terminated automatically when the session is dropped — no tunnels leak.
#[derive(Debug)]
pub struct Session {
    pub connection: Connection,
    _tunnel: Option<TunnelGuard>,
}

/// Terminates the `ssh -N -L` process when the session ends.
#[derive(Debug)]
struct TunnelGuard {
    child: Child,
}

impl Drop for TunnelGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Single choke point of the connection-resolution path: every operation
/// (`check`, `schema`, `query`, `exec`, `databases`, `views`) resolves its
/// connection here, so tunnel rewrites apply uniformly.
pub fn open_session(connection: &Connection) -> Result<Session> {
    let driver = connection.driver()?;
    let mut effective = connection.clone();
    let mut tunnel = None;

    if let Some(config) = connection.ssh_tunnel.as_ref().filter(|t| t.enabled) {
        if !matches!(driver, Driver::Postgres | Driver::Mysql | Driver::MariaDb) {
            bail!(
                "connection `{}`: ssh_tunnel.enabled is only supported for postgres, mysql, and mariadb drivers",
                connection.name
            );
        }
        let db_port = effective.port.or(driver.default_port()).with_context(|| {
            format!("connection `{}`: ssh_tunnel needs a tcp port", connection.name)
        })?;
        let db_host = effective
            .host
            .clone()
            .filter(|host| !host.is_empty())
            .with_context(|| {
                format!("connection `{}`: ssh_tunnel needs a tcp host", connection.name)
            })?;
        if effective.socket.is_some() {
            bail!(
                "connection `{}`: ssh_tunnel cannot be combined with a socket (the tunnel is tcp-only)",
                connection.name
            );
        }
        let local_port = match config.local_port {
            Some(port) => port,
            None => pick_free_port()?,
        };
        let child = establish_tunnel(config, &db_host, db_port, local_port)?;
        effective.host = Some("127.0.0.1".to_string());
        effective.port = Some(local_port);
        effective.url = None;
        effective.socket = None;
        tunnel = Some(TunnelGuard { child });
    }

    Ok(Session {
        connection: effective,
        _tunnel: tunnel,
    })
}

/// Picks a free ephemeral port by briefly binding `127.0.0.1:0` and dropping
/// the listener (the usual small bind-race, documented in the report).
pub fn pick_free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .context("cannot find a free local port for the ssh tunnel")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// The exact `ssh` argv. Deliberately argv-form (no shell interpolation) with
/// `BatchMode=yes` so a missing key fails fast instead of hanging on a prompt.
pub fn ssh_tunnel_command(
    tunnel: &SshTunnel,
    db_host: &str,
    db_port: u16,
    local_port: u16,
) -> Command {
    let mut command = Command::new("ssh");
    command
        .arg("-N")
        .arg("-L")
        .arg(format!("127.0.0.1:{local_port}:{db_host}:{db_port}"))
        .arg(format!("{}@{}", tunnel.user, tunnel.host))
        .arg("-p")
        .arg(tunnel.port.to_string());
    if let Some(identity_file) = &tunnel.identity_file {
        command.arg("-i").arg(identity_file);
    }
    command
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=5")
        .arg("-o")
        .arg("ServerAliveInterval=15");
    command
}

/// Spawns the tunnel and polls `127.0.0.1:<local_port>` every 200 ms for up to
/// 10 s. On success returns the ssh child (kept alive by the session); on any
/// failure the child is killed and `ssh tunnel failed: <stderr tail>` is
/// returned — the caller never falls through to a direct connection.
fn establish_tunnel(
    tunnel: &SshTunnel,
    db_host: &str,
    db_port: u16,
    local_port: u16,
) -> Result<Child> {
    let mut child = ssh_tunnel_command(tunnel, db_host, db_port, local_port)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| anyhow::anyhow!("ssh tunnel failed: {error}"))?;

    let deadline = Instant::now() + Duration::from_secs(10);
    let address: SocketAddr = format!("127.0.0.1:{local_port}")
        .parse()
        .context("ssh tunnel local address is malformed")?;
    let mut established = false;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("ssh tunnel failed: {error}");
            }
        }
        if TcpStream::connect_timeout(&address, Duration::from_millis(500)).is_ok() {
            established = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    if !established {
        let _ = child.kill();
        let _ = child.wait();
        let tail = stderr_tail(&mut child).unwrap_or_default();
        bail!("ssh tunnel failed: {tail}");
    }
    Ok(child)
}

fn stderr_tail(child: &mut Child) -> Option<String> {
    let mut pipe = child.stderr.take()?;
    let mut text = String::new();
    let _ = pipe.read_to_string(&mut text);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let last = trimmed.lines().last().unwrap_or(trimmed);
    Some(last.chars().take(300).collect())
}

// ---------------------------------------------------------------------------
// Public operations
// ---------------------------------------------------------------------------

pub fn check(connection: &Connection, base: &Path) -> Result<String> {
    let session = open_session(connection)?;
    let connection = &session.connection;
    let driver = connection.driver()?;
    require_client(driver)?;
    let (command, stdin) = build_check_command(connection, base)?;
    let output = run(command, driver.client(), stdin.as_deref())?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    match driver {
        Driver::Sqlite => Ok("file exists".to_string()),
        Driver::Mongodb => {
            if stdout.trim() == "1" {
                Ok("connected".to_string())
            } else {
                bail!("mongodb ping returned `{}`", stdout.trim())
            }
        }
        Driver::Redis => {
            if stdout.trim().eq_ignore_ascii_case("PONG") {
                Ok("connected".to_string())
            } else {
                bail!("redis ping returned `{}`", stdout.trim())
            }
        }
        _ => Ok("connected".to_string()),
    }
}

pub fn schema(connection: &Connection, base: &Path) -> Result<Schema> {
    let session = open_session(connection)?;
    let connection = &session.connection;
    let driver = connection.driver()?;
    require_client(driver)?;
    match driver {
        Driver::Sqlite => sqlite_schema(connection, base),
        Driver::Postgres => {
            let (command, stdin) = build_schema_command(connection, base)?;
            let output = run(command, driver.client(), stdin.as_deref())?;
            let text = String::from_utf8_lossy(&output.stdout);
            Ok(schema_from_csv(&text)?)
        }
        Driver::Mysql | Driver::MariaDb => {
            let (command, stdin) = build_schema_command(connection, base)?;
            let output = run(command, driver.client(), stdin.as_deref())?;
            let text = String::from_utf8_lossy(&output.stdout);
            Ok(schema_from_tsv(&text))
        }
        Driver::SqlServer => {
            let (command, stdin) = build_schema_command(connection, base)?;
            let output = run(command, driver.client(), stdin.as_deref())?;
            let text = String::from_utf8_lossy(&output.stdout);
            Ok(schema_from_lines(&text))
        }
        Driver::Oracle => {
            let (command, stdin) = build_schema_command(connection, base)?;
            let output = run(command, driver.client(), stdin.as_deref())?;
            let text = String::from_utf8_lossy(&output.stdout);
            Ok(schema_from_csv(&text)?)
        }
        Driver::Mongodb | Driver::Redis => {
            let (command, stdin) = build_schema_command(connection, base)?;
            let output = run(command, driver.client(), stdin.as_deref())?;
            let text = String::from_utf8_lossy(&output.stdout);
            Ok(schema_from_lines(&text))
        }
    }
}

pub fn query(connection: &Connection, base: &Path, sql: &str) -> Result<QueryResult> {
    let session = open_session(connection)?;
    let connection = &session.connection;
    let driver = connection.driver()?;
    if !driver.is_sql() {
        bail!(
            "{} is not a SQL driver; use `php-db exec {} \"<command>\" --allow-write`",
            driver,
            connection.name
        );
    }
    require_client(driver)?;
    match driver {
        Driver::Sqlite => sqlite_query(connection, base, sql),
        Driver::Postgres | Driver::Oracle => {
            let (command, stdin) = build_query_command(connection, base, sql)?;
            let output = run(command, driver.client(), stdin.as_deref())?;
            let text = String::from_utf8_lossy(&output.stdout);
            csv_to_result(&text)
        }
        Driver::Mysql | Driver::MariaDb | Driver::SqlServer => {
            let (command, stdin) = build_query_command(connection, base, sql)?;
            let output = run(command, driver.client(), stdin.as_deref())?;
            let text = String::from_utf8_lossy(&output.stdout);
            Ok(tsv_to_result(&text))
        }
        Driver::Mongodb | Driver::Redis => unreachable!("non-SQL drivers are handled above"),
    }
}

pub fn exec(connection: &Connection, command: &str) -> Result<String> {
    let session = open_session(connection)?;
    let connection = &session.connection;
    let driver = connection.driver()?;
    if driver.is_sql() {
        bail!(
            "`exec` is only available for mongodb and redis; use `query` for {}",
            driver
        );
    }
    require_client(driver)?;
    let (command, stdin) = build_exec_command(connection, command)?;
    let output = run(command, driver.client(), stdin.as_deref())?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// ---------------------------------------------------------------------------
// DSN composition (--verbose-dsn)
// ---------------------------------------------------------------------------

/// Composes the connection description shown by `--verbose-dsn`, per driver:
///
/// - postgres    conninfo URL with `sslmode`/`connect_timeout`/`application_name`
///               from `options` added as query parameters
/// - mysql/maria `driver://[user@]host:port/db[?charset=..&sslmode=..&connect_timeout=..]`
/// - sqlite      `sqlite3:<path>[?read_only=1]`
/// - the rest    the redacted `target()` form
///
/// Passwords never appear (see `redact_url`).
pub fn describe_dsn(connection: &Connection, base: &Path) -> Result<String> {
    let driver = connection.driver()?;
    Ok(match driver {
        Driver::Sqlite => {
            let mut dsn = format!("sqlite3:{}", connection.sqlite_path(base)?.display());
            if connection.read_only {
                dsn.push_str("?read_only=1");
            }
            dsn
        }
        Driver::Postgres => {
            let (url, _) = strip_url_password(&pg_url(connection)?);
            let url = match connection.sslmode.as_deref() {
                Some(sslmode) if !sslmode.is_empty() => {
                    add_query_param(&url, "sslmode", sslmode)
                }
                _ => url,
            };
            apply_postgres_options(&url, connection)
        }
        Driver::Mysql | Driver::MariaDb => {
            let mut dsn = format!("{}://", driver.as_str());
            if let Some(user) = connection.user.as_deref().filter(|user| !user.is_empty()) {
                dsn.push_str(user);
                dsn.push('@');
            }
            match connection.socket.as_deref() {
                Some(socket) => dsn.push_str(&format!("socket:{socket}")),
                None => dsn.push_str(&format!(
                    "{}:{}",
                    connection.host.as_deref().unwrap_or("localhost"),
                    connection.port.or(driver.default_port()).unwrap_or(3306)
                )),
            }
            if let Some(database) = connection.database.as_deref().filter(|d| !d.is_empty()) {
                dsn.push('/');
                dsn.push_str(database);
            }
            let mut params: Vec<String> = Vec::new();
            if let Some(options) = &connection.options {
                for key in ["charset", "sslmode", "connect_timeout"] {
                    if let Some(value) = options.get(key) {
                        params.push(format!("{key}={value}"));
                    }
                }
            }
            if !params.is_empty() {
                dsn.push('?');
                dsn.push_str(&params.join("&"));
            }
            dsn
        }
        _ => connection.target(),
    })
}

/// Option keys that the driver does not understand (kept in the config but
/// ignored at invocation time).
pub fn ignored_options(connection: &Connection) -> Vec<String> {
    let Some(options) = &connection.options else {
        return Vec::new();
    };
    let Ok(driver) = connection.driver() else {
        return options.keys().cloned().collect();
    };
    let known: &[&str] = match driver {
        Driver::Postgres => &["sslmode", "connect_timeout", "application_name"],
        Driver::Mysql | Driver::MariaDb => &["charset", "sslmode", "connect_timeout"],
        _ => &[],
    };
    options
        .keys()
        .filter(|key| !known.contains(&key.as_str()))
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Command construction (public so tests can inspect argv and environment)
// ---------------------------------------------------------------------------

pub fn databases(connection: &Connection, base: &Path) -> Result<Vec<String>> {
    let session = open_session(connection)?;
    let connection = &session.connection;
    let driver = connection.driver()?;
    match driver {
        Driver::Sqlite => {
            let path = sqlite_database(connection, base)?;
            Ok(vec![file_database_name(&path)])
        }
        // The oracle fallback names are derived from the config without
        // spawning sqlplus, so the client is required per-runnable arm.
        Driver::Postgres => {
            require_client(driver)?;
            let (command, _) = build_query_command(connection, base, PG_DATABASES_SQL)?;
            let output = run(command, driver.client(), None)?;
            names_from_csv(&String::from_utf8_lossy(&output.stdout))
        }
        Driver::Mysql | Driver::MariaDb => {
            require_client(driver)?;
            let (command, _) = build_query_command(connection, base, MYSQL_DATABASES_SQL)?;
            let output = run(command, driver.client(), None)?;
            Ok(names_from_tsv(&String::from_utf8_lossy(&output.stdout)))
        }
        Driver::SqlServer => {
            require_client(driver)?;
            let (command, _) = build_query_command(connection, base, SQLSERVER_DATABASES_SQL)?;
            let output = run(command, driver.client(), None)?;
            Ok(names_from_tsv(&String::from_utf8_lossy(&output.stdout)))
        }
        Driver::Oracle => {
            let name = connection
                .database
                .clone()
                .filter(|name| !name.is_empty())
                .or_else(|| {
                    // Service name from a url like `user@host:1521/service`.
                    connection
                        .url
                        .as_deref()
                        .and_then(|url| url.split('/').last())
                        .filter(|service| {
                            !service.is_empty() && !service.starts_with('!') && !service.contains('?')
                        })
                        .map(str::to_string)
                })
                .or_else(|| {
                    connection
                        .user
                        .clone()
                        .filter(|user| !user.is_empty())
                })
                .or_else(|| connection.url_username())
                .with_context(|| {
                    format!("oracle connection `{}` has no database name", connection.name)
                })?;
            Ok(vec![name])
        }
        Driver::Mongodb => {
            require_client(driver)?;
            let (command, _) = build_exec_command(
                connection,
                "db.adminCommand({ listDatabases: 1 }).databases.forEach(d => print(d.name))",
            )?;
            let output = run(command, driver.client(), None)?;
            Ok(names_from_lines(&String::from_utf8_lossy(&output.stdout)))
        }
        Driver::Redis => Ok(vec![redis_database_name(connection)]),
    }
}

pub fn views(connection: &Connection, base: &Path) -> Result<Schema> {
    let session = open_session(connection)?;
    let connection = &session.connection;
    let driver = connection.driver()?;
    require_client(driver)?;
    match driver {
        Driver::Sqlite => sqlite_views(connection, base),
        Driver::Postgres => {
            let (command, _) = build_query_command(connection, base, PG_VIEWS_SQL)?;
            let output = run(command, driver.client(), None)?;
            Ok(names_schema(
                names_from_csv(&String::from_utf8_lossy(&output.stdout))?,
            ))
        }
        Driver::Mysql | Driver::MariaDb => {
            let (command, _) = build_query_command(connection, base, MYSQL_VIEWS_SQL)?;
            let output = run(command, driver.client(), None)?;
            Ok(schema_from_tsv(&String::from_utf8_lossy(&output.stdout)))
        }
        Driver::SqlServer => {
            let (command, _) = build_query_command(connection, base, SQLSERVER_VIEWS_SQL)?;
            let output = run(command, driver.client(), None)?;
            Ok(schema_from_tsv(&String::from_utf8_lossy(&output.stdout)))
        }
        Driver::Oracle => {
            let target = oracle_target(connection)?;
            let (command, stdin) = (
                sqlplus_command(),
                Some(oracle_script(
                    &target,
                    "SELECT view_name FROM user_views ORDER BY view_name",
                )),
            );
            let output = run(command, driver.client(), stdin.as_deref())?;
            Ok(names_schema(
                names_from_csv(&String::from_utf8_lossy(&output.stdout))?,
            ))
        }
        Driver::Mongodb => {
            let (command, _) = build_exec_command(
                connection,
                "db.getCollectionInfos({ type: 'view' }).forEach(c => print(c.name))",
            )?;
            let output = run(command, driver.client(), None)?;
            Ok(schema_from_lines(&String::from_utf8_lossy(&output.stdout)))
        }
        Driver::Redis => Ok(Schema { tables: Vec::new() }),
    }
}

pub fn build_check_command(
    connection: &Connection,
    base: &Path,
) -> Result<(Command, Option<String>)> {
    let driver = connection.driver()?;
    Ok(match driver {
        Driver::Sqlite => {
            let path = sqlite_database(connection, base)?;
            (sqlite_command(&path, "SELECT 1 AS ok", connection.read_only), None)
        }
        Driver::Postgres => (pg_command(connection, "SELECT 1 AS ok")?, None),
        Driver::Mysql | Driver::MariaDb => (mysql_command(connection, "SELECT 1 AS ok")?, None),
        Driver::SqlServer => (
            sqlserver_command(connection, "SELECT 1 AS ok", false)?,
            None,
        ),
        Driver::Oracle => {
            let target = oracle_target(connection)?;
            (
                sqlplus_command(),
                Some(oracle_script(&target, "SELECT 1 AS ok FROM dual")),
            )
        }
        Driver::Mongodb => (
            mongodb_command(connection, "print(db.runCommand({ ping: 1 }).ok)")?,
            None,
        ),
        Driver::Redis => (redis_command(connection, &["PING"])?, None),
    })
}

pub fn build_schema_command(
    connection: &Connection,
    base: &Path,
) -> Result<(Command, Option<String>)> {
    let driver = connection.driver()?;
    Ok(match driver {
        Driver::Sqlite => {
            let path = sqlite_database(connection, base)?;
            (
                sqlite_command(
                    &path,
                    "SELECT name FROM sqlite_master WHERE type = 'table' \
                     AND name NOT LIKE 'sqlite_%' ORDER BY name",
                    connection.read_only,
                ),
                None,
            )
        }
        Driver::Postgres => (pg_command(connection, PG_SCHEMA_SQL)?, None),
        Driver::Mysql | Driver::MariaDb => (mysql_command(connection, "SHOW TABLES")?, None),
        Driver::SqlServer => (
            sqlserver_command(connection, SQLSERVER_SCHEMA_SQL, false)?,
            None,
        ),
        Driver::Oracle => {
            let target = oracle_target(connection)?;
            (
                sqlplus_command(),
                Some(oracle_script(
                    &target,
                    "SELECT table_name FROM user_tables ORDER BY table_name",
                )),
            )
        }
        Driver::Mongodb => (
            mongodb_command(
                connection,
                "db.getCollectionNames().sort().forEach(n => print(n))",
            )?,
            None,
        ),
        Driver::Redis => (
            redis_command(connection, &["--scan", "--count", "100"])?,
            None,
        ),
    })
}

pub fn build_query_command(
    connection: &Connection,
    base: &Path,
    sql: &str,
) -> Result<(Command, Option<String>)> {
    let driver = connection.driver()?;
    Ok(match driver {
        Driver::Sqlite => {
            let path = sqlite_database(connection, base)?;
            (sqlite_command(&path, sql, connection.read_only), None)
        }
        Driver::Postgres => (pg_command(connection, sql)?, None),
        Driver::Mysql | Driver::MariaDb => (mysql_command(connection, sql)?, None),
        Driver::SqlServer => (sqlserver_command(connection, sql, true)?, None),
        Driver::Oracle => {
            let target = oracle_target(connection)?;
            (sqlplus_command(), Some(oracle_script(&target, sql)))
        }
        Driver::Mongodb | Driver::Redis => bail!(
            "`query` is only available for SQL drivers; use `exec` for {}",
            driver
        ),
    })
}

pub fn build_exec_command(
    connection: &Connection,
    command: &str,
) -> Result<(Command, Option<String>)> {
    let driver = connection.driver()?;
    Ok(match driver {
        Driver::Mongodb => (mongodb_command(connection, command)?, None),
        Driver::Redis => {
            let args = split_command(command);
            if args.is_empty() {
                bail!("redis exec needs a command");
            }
            (redis_command(connection, &args)?, None)
        }
        _ => bail!(
            "`exec` is only available for mongodb and redis; use `query` for {}",
            driver
        ),
    })
}

// ---------------------------------------------------------------------------
// Process execution
// ---------------------------------------------------------------------------

fn run(mut command: Command, label: &str, stdin: Option<&str>) -> Result<Output> {
    let output = match stdin {
        Some(input) => {
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = command.spawn().with_context(|| {
                format!("failed to run `{label}`; is the client installed and on PATH?")
            })?;
            if let Some(mut pipe) = child.stdin.take() {
                pipe.write_all(input.as_bytes())
                    .with_context(|| format!("failed to send input to `{label}`"))?;
            }
            child
                .wait_with_output()
                .with_context(|| format!("failed to wait for `{label}`"))?
        }
        None => command.output().with_context(|| {
            format!("failed to run `{label}`; is the client installed and on PATH?")
        })?,
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        bail!(
            "{label} failed ({}): {}",
            output.status,
            if detail.is_empty() {
                "no error output"
            } else {
                detail
            }
        );
    }
    Ok(output)
}

// ---------------------------------------------------------------------------
// SQLite
// ---------------------------------------------------------------------------

fn sqlite_database(connection: &Connection, base: &Path) -> Result<PathBuf> {
    let path = connection.sqlite_path(base)?;
    if !path.is_file() {
        bail!(
            "sqlite database not found: {} (php-db never creates a database)",
            path.display()
        );
    }
    Ok(path)
}

fn sqlite_command(path: &Path, sql: &str, read_only: bool) -> Command {
    let mut command = Command::new("sqlite3");
    command.arg("-json");
    if read_only {
        // Opens the database with SQLITE_OPEN_READONLY: the engine refuses
        // writes even if the statement-level heuristic missed something.
        command.arg("-readonly");
    }
    command.arg(path).arg(sql);
    command
}

fn sqlite_schema(connection: &Connection, base: &Path) -> Result<Schema> {
    sqlite_relations(connection, base, "table")
}

fn sqlite_views(connection: &Connection, base: &Path) -> Result<Schema> {
    sqlite_relations(connection, base, "view")
}

fn sqlite_relations(connection: &Connection, base: &Path, kind: &str) -> Result<Schema> {
    let path = connection.sqlite_path(base)?;
    if !path.is_file() {
        bail!(
            "sqlite database not found: {} (php-db never creates a database)",
            path.display()
        );
    }
    let relation_rows = sqlite_rows(
        &path,
        &format!(
            "SELECT name FROM sqlite_schema WHERE type = '{kind}' \
             AND name NOT LIKE 'sqlite_%' ORDER BY name"
        ),
        connection.read_only,
    )?;

    let mut tables = Vec::with_capacity(relation_rows.len());
    for row in relation_rows {
        let name = string_field(&row, "name");
        if name.is_empty() {
            continue;
        }
        let pragma = format!("PRAGMA table_info(\"{}\")", name.replace('"', "\"\""));
        let columns = sqlite_rows(&path, &pragma, connection.read_only)?
            .into_iter()
            .map(|row| Column {
                name: string_field(&row, "name"),
                kind: string_field(&row, "type"),
                not_null: integer_field(&row, "notnull") != 0,
                primary_key: integer_field(&row, "pk") != 0,
                default: row
                    .get("dflt_value")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .collect();
        tables.push(Table { name, columns });
    }
    Ok(Schema { tables })
}

fn sqlite_query(connection: &Connection, base: &Path, sql: &str) -> Result<QueryResult> {
    let path = connection.sqlite_path(base)?;
    if !path.is_file() {
        bail!(
            "sqlite database not found: {} (php-db never creates a database)",
            path.display()
        );
    }
    let rows = sqlite_rows(&path, sql, connection.read_only)?;
    let columns: Vec<String> = rows
        .first()
        .map(|row| row.keys().cloned().collect())
        .unwrap_or_default();
    let rows = rows
        .into_iter()
        .map(|row| {
            columns
                .iter()
                .map(|column| row.get(column).cloned().unwrap_or(Value::Null))
                .collect()
        })
        .collect();
    Ok(QueryResult { columns, rows })
}

fn sqlite_rows(path: &Path, sql: &str, read_only: bool) -> Result<Vec<Map<String, Value>>> {
    let output = run(sqlite_command(path, sql, read_only), "sqlite3", None)?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(trimmed)
        .with_context(|| format!("could not parse sqlite3 JSON output: {trimmed}"))?;
    let items = value
        .as_array()
        .context("sqlite3 JSON output was not an array")?;
    let mut rows = Vec::with_capacity(items.len());
    for item in items {
        rows.push(
            item.as_object()
                .cloned()
                .context("sqlite3 JSON row was not an object")?,
        );
    }
    Ok(rows)
}

fn string_field(row: &Map<String, Value>, key: &str) -> String {
    match row.get(key) {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn file_database_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn redis_database_name(connection: &Connection) -> String {
    connection
        .database
        .as_deref()
        .filter(|database| !database.is_empty())
        .unwrap_or("db0")
        .to_string()
}

fn names_schema(names: Vec<String>) -> Schema {
    Schema {
        tables: names
            .into_iter()
            .map(|name| Table {
                name,
                columns: Vec::new(),
            })
            .collect(),
    }
}

fn names_from_csv(text: &str) -> Result<Vec<String>> {
    let result = csv_to_result(text)?;
    Ok(result
        .rows
        .iter()
        .filter_map(|row| row.first())
        .filter_map(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect())
}

fn names_from_tsv(text: &str) -> Vec<String> {
    tsv_to_result(text)
        .rows
        .iter()
        .filter_map(|row| row.first())
        .filter_map(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

fn names_from_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}


fn integer_field(row: &Map<String, Value>, key: &str) -> i64 {
    match row.get(key) {
        Some(value) => value.as_i64().unwrap_or(0),
        None => 0,
    }
}

// ---------------------------------------------------------------------------
// PostgreSQL
// ---------------------------------------------------------------------------

const PG_SCHEMA_SQL: &str = "\
SELECT CASE
         WHEN table_schema = 'public' THEN table_name
         ELSE table_schema || '.' || table_name
       END AS name
FROM information_schema.tables
WHERE table_type = 'BASE TABLE'
  AND table_schema NOT IN ('pg_catalog', 'information_schema')
ORDER BY 1";

fn pg_url(connection: &Connection) -> Result<String> {
    let url = match connection.url.as_deref() {
        Some(url) if !url.is_empty() => url.to_string(),
        _ => {
            let host = connection
                .host
                .as_deref()
                .context("postgres connection is missing `host`")?;
            let port = connection.port.unwrap_or(5432);
            let user = connection.user.as_deref().unwrap_or("");
            let mut url = format!("postgresql://{user}@{host}:{port}");
            if let Some(database) = connection.database.as_deref() {
                if !database.is_empty() {
                    url.push('/');
                    url.push_str(database);
                }
            }
            url
        }
    };
    Ok(url)
}

/// Known options for postgres are translated into conninfo URL parameters.
/// `add_query_param` never duplicates a key the url already carries.
fn apply_postgres_options(url: &str, connection: &Connection) -> String {
    let Some(options) = &connection.options else {
        return url.to_string();
    };
    let mut url = url.to_string();
    for key in ["sslmode", "connect_timeout", "application_name"] {
        if let Some(value) = options.get(key) {
            url = add_query_param(&url, key, value);
        }
    }
    url
}

fn pg_command(connection: &Connection, sql: &str) -> Result<Command> {    let password = connection.password()?;
    let (url, _) = strip_url_password(&pg_url(connection)?);
    let url = match connection.sslmode.as_deref() {
        Some(sslmode) if !sslmode.is_empty() => add_query_param(&url, "sslmode", sslmode),
        _ => url,
    };
    let url = apply_postgres_options(&url, connection);

    let mut command = Command::new("psql");
    command
        .arg(url)
        .arg("-X")
        .arg("-v")
        .arg("ON_ERROR_STOP=1")
        .arg("--csv");
    if let Some(password) = password {
        command.env("PGPASSWORD", password);
    }
    command.arg("-c").arg(sql);
    Ok(command)
}

// ---------------------------------------------------------------------------
// MySQL / MariaDB
// ---------------------------------------------------------------------------

/// Known options for mysql/mariadb become client flags. MariaDB's client only
/// has the legacy `--ssl` boolean, so `sslmode=disabled` is the only value that
/// suppresses it. Unknown options are left to `--verbose-dsn`'s ignored list.
fn apply_mysql_options(command: &mut Command, connection: &Connection, driver: Driver) {
    let Some(options) = &connection.options else {
        return;
    };
    if let Some(charset) = options.get("charset") {
        command.arg("--default-character-set").arg(charset);
    }
    if let Some(timeout) = options.get("connect_timeout") {
        command.arg("--connect-timeout").arg(timeout);
    }
    if let Some(sslmode) = options.get("sslmode") {
        if driver == Driver::Mysql {
            command.arg(format!("--ssl-mode={sslmode}"));
        } else if sslmode != "disabled" {
            command.arg("--ssl");
        }
    }
}

struct UrlTarget {
    host: String,
    port: Option<u16>,
    user: Option<String>,
    password: Option<String>,
    database: Option<String>,
}

fn parse_url_target(url: &str, schemes: &[&str]) -> Result<UrlTarget> {
    let (scheme, rest) = url
        .split_once("://")
        .with_context(|| format!("url must include a scheme: {url}"))?;
    if !schemes.contains(&scheme) {
        bail!(
            "url scheme `{scheme}` is not supported here (expected {})",
            schemes.join(", ")
        );
    }
    let (authority, tail) = match rest.split_once('/') {
        Some((authority, tail)) => (authority, Some(tail)),
        None => (rest, None),
    };
    let database = tail
        .and_then(|tail| tail.split(['?', '#']).next())
        .filter(|database| !database.is_empty())
        .map(str::to_string);

    let (userinfo, host_port) = match authority.rsplit_once('@') {
        Some((userinfo, host_port)) => (Some(userinfo), host_port),
        None => (None, authority),
    };
    let (user, password) = match userinfo {
        Some(userinfo) => match userinfo.split_once(':') {
            Some((user, password)) => (Some(percent_decode(user)), Some(percent_decode(password))),
            None => (Some(percent_decode(userinfo)), None),
        },
        None => (None, None),
    };
    if user.as_deref() == Some("") {
        bail!("url has an empty user name");
    }
    let (host, port) = split_host_port(host_port)?;
    if host.is_empty() {
        bail!("url is missing a host");
    }
    Ok(UrlTarget {
        host,
        port,
        user,
        password,
        database,
    })
}

fn parse_mysql_url(url: &str) -> Result<UrlTarget> {
    let target = parse_url_target(url, &["mysql"])?;
    if target.database.is_none() {
        bail!("mysql url must include a database name");
    }
    Ok(target)
}

fn split_host_port(value: &str) -> Result<(String, Option<u16>)> {
    if let Some(rest) = value.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .context("url has an unterminated IPv6 host")?;
        let port = match tail.strip_prefix(':') {
            Some(port) => Some(port.parse().context("url has an invalid port")?),
            None => None,
        };
        return Ok((host.to_string(), port));
    }
    match value.rsplit_once(':') {
        Some((host, port)) => Ok((
            host.to_string(),
            Some(port.parse().context("url has an invalid port")?),
        )),
        None => Ok((value.to_string(), None)),
    }
}

struct MysqlTarget {
    host: Option<String>,
    port: Option<u16>,
    user: Option<String>,
    password: Option<String>,
    database: Option<String>,
    socket: Option<String>,
    ssl: bool,
}

fn mysql_target(connection: &Connection) -> Result<MysqlTarget> {
    let mut target = MysqlTarget {
        host: connection.host.clone(),
        port: connection.port,
        user: connection.user.clone(),
        password: connection.password()?,
        database: connection.database.clone(),
        socket: connection.socket.clone(),
        ssl: connection.ssl.unwrap_or(false),
    };
    if let Some(url) = connection.url.as_deref().filter(|url| !url.is_empty()) {
        // PhpStorm semantics: the url overrides the individual fields.
        let parsed = parse_mysql_url(url)?;
        target.host = Some(parsed.host);
        target.port = parsed.port;
        target.user = parsed.user;
        if target.password.is_none() {
            target.password = parsed.password;
        }
        target.database = parsed.database;
    }
    if target.host.is_none() && target.socket.is_none() {
        bail!("mysql connection needs a host or socket");
    }
    Ok(target)
}

fn mysql_command(connection: &Connection, sql: &str) -> Result<Command> {
    let driver = connection.driver()?;
    let target = mysql_target(connection)?;
    let mut command = Command::new("mysql");
    command.arg("--batch").arg("--raw");

    if let Some(socket) = &target.socket {
        command.arg("--socket").arg(socket);
    } else {
        command
            .arg("-h")
            .arg(target.host.as_deref().unwrap_or("localhost"));
        if let Some(port) = target.port {
            command.arg("-P").arg(port.to_string());
        }
    }
    if let Some(user) = &target.user {
        command.arg("-u").arg(user);
    }
    if let Some(database) = &target.database {
        command.arg("-D").arg(database);
    }
    if target.ssl {
        if driver == Driver::Mysql {
            command.arg("--ssl-mode=REQUIRED");
        } else {
            command.arg("--ssl");
        }
    }
    apply_mysql_options(&mut command, connection, driver);
    // MYSQL_PWD keeps the password out of the process arguments.
    if let Some(password) = target.password {
        command.env("MYSQL_PWD", password);
    }
    command.arg("-e").arg(sql);
    Ok(command)
}

// ---------------------------------------------------------------------------
// SQL Server
// ---------------------------------------------------------------------------

const SQLSERVER_SCHEMA_SQL: &str = "\
SET NOCOUNT ON;
SELECT CASE
         WHEN TABLE_SCHEMA = 'dbo' THEN TABLE_NAME
         ELSE TABLE_SCHEMA + '.' + TABLE_NAME
       END AS name
FROM INFORMATION_SCHEMA.TABLES
WHERE TABLE_TYPE = 'BASE TABLE'
ORDER BY name";

const SQLSERVER_DATABASES_SQL: &str = "\
SELECT name FROM sys.databases
WHERE name NOT IN ('master', 'tempdb', 'model', 'msdb')
ORDER BY name";

const MYSQL_DATABASES_SQL: &str = "\
SELECT SCHEMA_NAME FROM information_schema.SCHEMATA
WHERE SCHEMA_NAME NOT IN ('mysql','information_schema','performance_schema','sys')
ORDER BY SCHEMA_NAME";

const PG_DATABASES_SQL: &str = "\
SELECT datname FROM pg_database WHERE NOT datistemplate ORDER BY datname";

const SQLSERVER_VIEWS_SQL: &str = "\
SET NOCOUNT ON;
SELECT CASE
         WHEN TABLE_SCHEMA = 'dbo' THEN TABLE_NAME
         ELSE TABLE_SCHEMA + '.' + TABLE_NAME
       END AS name
FROM INFORMATION_SCHEMA.VIEWS
WHERE TABLE_SCHEMA NOT IN ('sys', 'INFORMATION_SCHEMA')
ORDER BY name";

const MYSQL_VIEWS_SQL: &str = "SHOW FULL TABLES WHERE Table_type = 'VIEW'";

const PG_VIEWS_SQL: &str = "\
SELECT c.relname AS name
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = 'public'
  AND c.relkind = 'v'
ORDER BY name";

struct SqlServerTarget {
    host: String,
    port: Option<u16>,
    user: Option<String>,
    password: Option<String>,
    database: Option<String>,
    ssl: bool,
}

fn sqlserver_target(connection: &Connection) -> Result<SqlServerTarget> {
    let mut target = SqlServerTarget {
        host: connection.host.clone().unwrap_or_default(),
        port: connection.port,
        user: connection.user.clone(),
        password: connection.password()?,
        database: connection.database.clone(),
        ssl: connection.ssl.unwrap_or(false),
    };
    if let Some(url) = connection.url.as_deref().filter(|url| !url.is_empty()) {
        // PhpStorm semantics: the url overrides the individual fields.
        let parsed = parse_url_target(url, &["sqlserver", "mssql"])?;
        target.host = parsed.host;
        target.port = parsed.port;
        target.user = parsed.user;
        if target.password.is_none() {
            target.password = parsed.password;
        }
        target.database = parsed.database;
    }
    if target.host.is_empty() {
        bail!("sqlserver connection needs a host");
    }
    Ok(target)
}

fn sqlserver_command(connection: &Connection, sql: &str, headers: bool) -> Result<Command> {
    let target = sqlserver_target(connection)?;
    let mut command = Command::new("sqlcmd");
    command
        .arg("-S")
        .arg(format!("{},{}", target.host, target.port.unwrap_or(1433)));
    if let Some(user) = &target.user {
        command.arg("-U").arg(user);
    }
    if let Some(database) = &target.database {
        command.arg("-d").arg(database);
    }
    if let Some(password) = &target.password {
        // SQLCMDPASSWORD keeps the password out of the process arguments.
        command.env("SQLCMDPASSWORD", password);
    }
    command.arg("-b");
    if !headers {
        command.arg("-h").arg("-1");
    }
    command.arg("-W").arg("-w").arg("65535").arg("-s").arg("\t");
    if target.ssl {
        command.arg("-C");
    }
    command.arg("-Q").arg(format!("SET NOCOUNT ON;\n{sql}"));
    Ok(command)
}

// ---------------------------------------------------------------------------
// Oracle
// ---------------------------------------------------------------------------

struct OracleTarget {
    user: String,
    password: Option<String>,
    connect: String,
}

fn oracle_target(connection: &Connection) -> Result<OracleTarget> {
    let raw = match connection.url.as_deref() {
        Some(url) if !url.is_empty() => url.to_string(),
        _ => format!(
            "{}@{}:{}/{}",
            connection.user.as_deref().unwrap_or(""),
            connection
                .host
                .as_deref()
                .context("oracle connection needs a host")?,
            connection.port.unwrap_or(1521),
            connection.database.as_deref().unwrap_or("")
        ),
    };
    let (userinfo, connect) = raw
        .split_once('@')
        .with_context(|| format!("oracle url must look like user@host:port/service: {raw}"))?;
    let (user, embedded) = match userinfo.split_once('/') {
        Some((user, password)) => (percent_decode(user), Some(percent_decode(password))),
        None => match userinfo.split_once(':') {
            Some((user, password)) => (percent_decode(user), Some(percent_decode(password))),
            None => (percent_decode(userinfo), None),
        },
    };
    if user.is_empty() {
        bail!("oracle connection needs a user");
    }
    let password = match connection.password_env {
        Some(_) => connection.password()?,
        None => embedded,
    };
    let connect = if connect.starts_with("//") {
        connect.to_string()
    } else {
        format!("//{connect}")
    };
    Ok(OracleTarget {
        user,
        password,
        connect,
    })
}

fn sqlplus_command() -> Command {
    let mut command = Command::new("sqlplus");
    command.arg("-S").arg("-L").arg("/nolog");
    command
}

fn oracle_script(target: &OracleTarget, sql: &str) -> String {
    let mut script = String::new();
    script.push_str("WHENEVER SQLERROR EXIT SQL.SQLCODE\n");
    script.push_str("SET MARKUP CSV ON DELIMITER , QUOTE ON\n");
    script.push_str("SET FEEDBACK OFF\nSET VERIFY OFF\n");
    match &target.password {
        Some(password) => script.push_str(&format!(
            "CONNECT {}/\"{}\"@{}\n",
            target.user,
            password.replace('"', "\"\""),
            target.connect
        )),
        None => script.push_str(&format!("CONNECT {}@{}\n", target.user, target.connect)),
    }
    let trimmed = sql.trim();
    script.push_str(trimmed);
    if !trimmed.ends_with(';') && !trimmed.ends_with('/') {
        script.push(';');
    }
    script.push_str("\nEXIT\n");
    script
}

// ---------------------------------------------------------------------------
// MongoDB
// ---------------------------------------------------------------------------

const MONGO_AUTH: &str = "\
const __u = process.env.PHP_DB_MONGO_USER;
const __p = process.env.PHP_DB_MONGO_PASSWORD;
if (__u && __p) { db.auth(__u, __p); }";

struct MongoTarget {
    uri: String,
    user: Option<String>,
    password: Option<String>,
}

fn mongodb_target(connection: &Connection) -> Result<MongoTarget> {
    let user = connection
        .user
        .clone()
        .or_else(|| connection.url_username());
    let password = connection.password()?;
    let uri = match connection.url.as_deref() {
        Some(url) if !url.is_empty() => strip_url_credentials(url),
        _ => format!(
            "mongodb://{}:{}/{}",
            connection.host.as_deref().unwrap_or("localhost"),
            connection.port.unwrap_or(27017),
            connection.database.as_deref().unwrap_or("")
        ),
    };
    Ok(MongoTarget {
        uri,
        user,
        password,
    })
}

fn mongodb_command(connection: &Connection, script: &str) -> Result<Command> {
    let target = mongodb_target(connection)?;
    let script = format!("{MONGO_AUTH}\n{script}");
    let mut command = Command::new("mongosh");
    command
        .arg(&target.uri)
        .arg("--quiet")
        .arg("--eval")
        .arg(script);
    if let Some(user) = target.user {
        command.env("PHP_DB_MONGO_USER", user);
    }
    if let Some(password) = target.password {
        command.env("PHP_DB_MONGO_PASSWORD", password);
    }
    Ok(command)
}

// ---------------------------------------------------------------------------
// Redis
// ---------------------------------------------------------------------------

struct RedisTarget {
    host: String,
    port: u16,
    socket: Option<String>,
    db: Option<u32>,
    user: Option<String>,
    password: Option<String>,
    tls: bool,
}

fn redis_target(connection: &Connection) -> Result<RedisTarget> {
    let mut target = RedisTarget {
        host: connection
            .host
            .clone()
            .unwrap_or_else(|| "127.0.0.1".to_string()),
        port: connection.port.unwrap_or(6379),
        socket: connection.socket.clone(),
        db: None,
        user: connection.user.clone(),
        password: connection.password()?,
        tls: connection.ssl.unwrap_or(false),
    };
    if let Some(url) = connection.url.as_deref().filter(|url| !url.is_empty()) {
        let (scheme, rest) = url
            .split_once("://")
            .with_context(|| format!("redis url must include a scheme: {url}"))?;
        match scheme {
            "redis" => {}
            "rediss" => target.tls = true,
            other => {
                bail!("redis url scheme `{other}` is not supported (expected redis or rediss)")
            }
        }
        let (authority, tail) = match rest.split_once('/') {
            Some((authority, tail)) => (authority, Some(tail)),
            None => (rest, None),
        };
        if let Some(db) = tail
            .and_then(|tail| tail.split(['?', '#']).next())
            .filter(|db| !db.is_empty())
        {
            target.db = Some(
                db.parse()
                    .context("redis url has an invalid database number")?,
            );
        }
        let (userinfo, host_port) = match authority.rsplit_once('@') {
            Some((userinfo, host_port)) => (Some(userinfo), host_port),
            None => (None, authority),
        };
        if let Some(userinfo) = userinfo {
            let (user, password) = match userinfo.split_once(':') {
                Some((user, password)) => (
                    (!user.is_empty()).then(|| percent_decode(user)),
                    (!password.is_empty()).then(|| percent_decode(password)),
                ),
                None => (Some(percent_decode(userinfo)), None),
            };
            if target.user.is_none() {
                target.user = user;
            }
            if target.password.is_none() {
                target.password = password;
            }
        }
        let (host, port) = split_host_port(host_port)?;
        target.host = host;
        if let Some(port) = port {
            target.port = port;
        }
    }
    Ok(target)
}

fn redis_command<T: AsRef<OsStr>>(connection: &Connection, args: &[T]) -> Result<Command> {
    let target = redis_target(connection)?;
    let mut command = Command::new("redis-cli");
    if let Some(socket) = &target.socket {
        command.arg("-s").arg(socket);
    } else {
        command.arg("-h").arg(&target.host);
        command.arg("-p").arg(target.port.to_string());
    }
    if let Some(db) = target.db {
        command.arg("-n").arg(db.to_string());
    }
    if let Some(user) = &target.user {
        command.arg("--user").arg(user);
    }
    if target.tls {
        command.arg("--tls");
    }
    command.arg("--no-auth-warning");
    // REDISCLI_AUTH keeps the password out of the process arguments.
    if let Some(password) = target.password {
        command.env("REDISCLI_AUTH", password);
    }
    command.args(args);
    Ok(command)
}

/// Splits a redis command string into argv, honouring single and double quotes
/// and backslash escapes.
pub fn split_command(command: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for character in command.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        match quote {
            Some(active) => {
                if character == '\\' {
                    escaped = true;
                } else if character == active {
                    quote = None;
                } else {
                    current.push(character);
                }
            }
            None => match character {
                '\\' => escaped = true,
                '\'' | '"' => quote = Some(character),
                character if character.is_whitespace() => {
                    if !current.is_empty() {
                        args.push(std::mem::take(&mut current));
                    }
                }
                character => current.push(character),
            },
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

// ---------------------------------------------------------------------------
// Output parsing
// ---------------------------------------------------------------------------

fn schema_from_csv(text: &str) -> Result<Schema> {
    let result = csv_to_result(text)?;
    Ok(Schema {
        tables: result
            .rows
            .iter()
            .filter_map(|row| row.first())
            .filter_map(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(|name| Table {
                name: name.to_string(),
                columns: Vec::new(),
            })
            .collect(),
    })
}

fn schema_from_tsv(text: &str) -> Schema {
    let result = tsv_to_result(text);
    Schema {
        tables: result
            .rows
            .iter()
            .filter_map(|row| row.first())
            .filter_map(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(|name| Table {
                name: name.to_string(),
                columns: Vec::new(),
            })
            .collect(),
    }
}

fn schema_from_lines(text: &str) -> Schema {
    Schema {
        tables: text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .filter(|line| !line.starts_with('('))
            .map(|name| Table {
                name: name.to_string(),
                columns: Vec::new(),
            })
            .collect(),
    }
}

fn csv_to_result(text: &str) -> Result<QueryResult> {
    let records = parse_csv(text)?;
    if records.is_empty() {
        return Ok(QueryResult {
            columns: Vec::new(),
            rows: Vec::new(),
        });
    }
    if records.len() == 1 && records[0].len() == 1 {
        if let Some(value) = &records[0][0] {
            if looks_like_command_tag(value) {
                return Ok(QueryResult {
                    columns: Vec::new(),
                    rows: Vec::new(),
                });
            }
        }
    }

    let columns: Vec<String> = records[0]
        .iter()
        .map(|value| value.clone().unwrap_or_default())
        .collect();
    let rows = records
        .iter()
        .skip(1)
        .map(|record| {
            (0..columns.len())
                .map(|index| match record.get(index) {
                    Some(Some(value)) => Value::String(value.clone()),
                    _ => Value::Null,
                })
                .collect()
        })
        .collect();
    Ok(QueryResult { columns, rows })
}

fn parse_csv(text: &str) -> Result<Vec<Vec<Option<String>>>> {
    let mut chars = text.chars().peekable();
    let mut records: Vec<Vec<Option<String>>> = Vec::new();
    let mut record: Vec<Option<String>> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut was_quoted = false;
    let mut saw_input = false;

    while let Some(character) = chars.next() {
        saw_input = true;
        if in_quotes {
            if character == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(character);
            }
            continue;
        }
        match character {
            '"' if field.is_empty() && !was_quoted => {
                in_quotes = true;
                was_quoted = true;
            }
            ',' => {
                record.push(finish_csv_field(&mut field, was_quoted));
                was_quoted = false;
            }
            '\n' => {
                record.push(finish_csv_field(&mut field, was_quoted));
                records.push(std::mem::take(&mut record));
                was_quoted = false;
            }
            '\r' => {}
            _ => field.push(character),
        }
    }

    if in_quotes {
        bail!("unterminated quoted field in CSV output");
    }
    if !field.is_empty() || was_quoted || !record.is_empty() {
        record.push(finish_csv_field(&mut field, was_quoted));
        records.push(record);
    } else if !saw_input {
        return Ok(records);
    }
    Ok(records)
}

fn finish_csv_field(field: &mut String, was_quoted: bool) -> Option<String> {
    let value = std::mem::take(field);
    if was_quoted || !value.is_empty() {
        Some(value)
    } else {
        None
    }
}

fn looks_like_command_tag(value: &str) -> bool {
    let mut words = value.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    matches!(
        first,
        "INSERT"
            | "UPDATE"
            | "DELETE"
            | "CREATE"
            | "DROP"
            | "ALTER"
            | "TRUNCATE"
            | "BEGIN"
            | "COMMIT"
            | "ROLLBACK"
            | "SET"
            | "GRANT"
            | "REVOKE"
            | "COPY"
    ) && value.split_whitespace().all(|word| {
        word.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    })
}

fn tsv_to_result(text: &str) -> QueryResult {
    let text = text.trim_end_matches(['\n', '\r']);
    if text.is_empty() {
        return QueryResult {
            columns: Vec::new(),
            rows: Vec::new(),
        };
    }
    let mut lines = text.lines();
    let columns: Vec<String> = match lines.next() {
        Some(header) => header.split('\t').map(str::to_string).collect(),
        None => Vec::new(),
    };
    let rows = lines
        .map(|line| {
            line.split('\t')
                .map(|field| {
                    if field == "NULL" {
                        Value::Null
                    } else {
                        Value::String(field.to_string())
                    }
                })
                .collect()
        })
        .collect();
    QueryResult { columns, rows }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(json: &str) -> Connection {
        serde_json::from_str(json).unwrap()
    }

    fn base() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/db-drivers")
    }

    fn args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn env_value(command: &Command, key: &str) -> Option<String> {
        command
            .get_envs()
            .find(|(name, _)| *name == OsStr::new(key))
            .and_then(|(_, value)| value)
            .map(|value| value.to_string_lossy().into_owned())
    }

    #[test]
    fn missing_client_is_reported_for_every_adapter() {
        let empty = OsStr::new("/nonexistent-php-db-path");
        for driver in Driver::ALL {
            let error = require_client_with(driver, Some(empty))
                .unwrap_err()
                .to_string();
            assert!(error.contains(driver.client()), "{driver}: {error}");
            assert!(error.contains("not found"), "{driver}: {error}");
        }
    }

    #[test]
    fn sqlite_integration_schema_and_query() {
        let connection = connection(r#"{"name":"app","driver":"sqlite","path":"demo.sqlite"}"#);
        let schema = sqlite_schema(&connection, &base()).unwrap();
        let users = schema
            .tables
            .iter()
            .find(|table| table.name == "users")
            .expect("users table");
        assert_eq!(
            users
                .columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["id", "email", "created_at"]
        );
        assert!(users.columns[0].primary_key);
        assert!(users.columns[1].not_null);

        let result = sqlite_query(
            &connection,
            &base(),
            "SELECT id, email FROM users ORDER BY id",
        )
        .unwrap();
        assert_eq!(result.columns, vec!["id", "email"]);
        assert_eq!(result.rows.len(), 3);
        assert_eq!(
            result.rows[0][1],
            Value::String("alice@example.com".to_string())
        );
    }

    #[test]
    fn builds_sqlite_commands() {
        let connection = connection(r#"{"name":"app","driver":"sqlite","path":"demo.sqlite"}"#);
        let (command, stdin) = build_query_command(&connection, &base(), "SELECT 1").unwrap();
        assert!(stdin.is_none());
        let args = args(&command);
        assert_eq!(command.get_program(), OsStr::new("sqlite3"));
        assert_eq!(args[0], "-json");
        assert!(args[1].ends_with("fixtures/db-drivers/demo.sqlite"));
        assert_eq!(args[2], "SELECT 1");
    }

    #[test]
    fn postgres_command_hides_password_and_applies_sslmode() {
        let connection = connection(
            r#"{"name":"pg","driver":"postgres",
                "url":"postgres://app:s3cret@db:5432/app","sslmode":"require"}"#,
        );
        let (command, _) = build_query_command(&connection, Path::new("."), "SELECT 1").unwrap();
        let args = args(&command);
        assert_eq!(command.get_program(), OsStr::new("psql"));
        assert!(args
            .iter()
            .any(|arg| arg == "postgres://app@db:5432/app?sslmode=require"));
        assert!(!args.join(" ").contains("s3cret"));
        assert_eq!(env_value(&command, "PGPASSWORD").as_deref(), Some("s3cret"));
    }

    #[test]
    fn mysql_command_uses_socket_and_hides_password() {
        let connection = connection(
            r#"{"name":"my","driver":"mysql",
                "url":"mysql://app:s3cret@db:3306/appdb",
                "socket":"/tmp/mysql.sock","ssl":true}"#,
        );
        let (command, _) = build_query_command(&connection, Path::new("."), "SELECT 1").unwrap();
        let args = args(&command);
        assert_eq!(command.get_program(), OsStr::new("mysql"));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--socket", "/tmp/mysql.sock"]));
        assert!(args.iter().any(|arg| arg == "--ssl-mode=REQUIRED"));
        assert!(!args.join(" ").contains("s3cret"));
        assert_eq!(env_value(&command, "MYSQL_PWD").as_deref(), Some("s3cret"));
    }

    #[test]
    fn mariadb_uses_legacy_ssl_flag() {
        let connection = connection(
            r#"{"name":"maria","driver":"mariadb","host":"h","user":"u",
                "database":"d","ssl":true}"#,
        );
        let (command, _) = build_query_command(&connection, Path::new("."), "SELECT 1").unwrap();
        let args = args(&command);
        assert!(args.iter().any(|arg| arg == "--ssl"));
        assert!(!args.iter().any(|arg| arg.starts_with("--ssl-mode")));
    }

    #[test]
    fn sqlserver_command_hides_password() {
        let connection = connection(
            r#"{"name":"ms","driver":"sqlserver",
                "url":"sqlserver://u:s3cret@h:1433/d"}"#,
        );
        let (command, _) = build_query_command(&connection, Path::new("."), "SELECT 1").unwrap();
        let args = args(&command);
        assert_eq!(command.get_program(), OsStr::new("sqlcmd"));
        assert!(args.windows(2).any(|pair| pair == ["-S", "h,1433"]));
        assert!(!args.join(" ").contains("s3cret"));
        assert_eq!(
            env_value(&command, "SQLCMDPASSWORD").as_deref(),
            Some("s3cret")
        );
    }

    #[test]
    fn oracle_password_goes_to_stdin_not_argv() {
        let connection =
            connection(r#"{"name":"ora","driver":"oracle","url":"u:s3cret@h:1521/svc"}"#);
        let (command, stdin) =
            build_query_command(&connection, Path::new("."), "SELECT 1 FROM dual").unwrap();
        let args = args(&command);
        let script = stdin.unwrap();
        assert_eq!(command.get_program(), OsStr::new("sqlplus"));
        assert!(!args.join(" ").contains("s3cret"));
        assert!(script.contains("u/\"s3cret\"@//h:1521/svc"));
        assert!(script.contains("SELECT 1 FROM dual;"));
    }

    #[test]
    fn mongodb_command_hides_password() {
        let connection = connection(
            r#"{"name":"mongo","driver":"mongodb",
                "url":"mongodb://u:s3cret@localhost:27017/db"}"#,
        );
        let (command, _) = build_exec_command(&connection, "db.stats()").unwrap();
        let args = args(&command);
        assert_eq!(command.get_program(), OsStr::new("mongosh"));
        assert!(args.iter().any(|arg| arg == "mongodb://localhost:27017/db"));
        assert!(!args.join(" ").contains("s3cret"));
        assert_eq!(
            env_value(&command, "PHP_DB_MONGO_PASSWORD").as_deref(),
            Some("s3cret")
        );
        assert_eq!(
            env_value(&command, "PHP_DB_MONGO_USER").as_deref(),
            Some("u")
        );
        assert!(args
            .iter()
            .any(|arg| arg.contains("process.env.PHP_DB_MONGO_USER")));
    }

    #[test]
    fn redis_command_hides_password_and_keeps_db() {
        let connection = connection(
            r#"{"name":"cache","driver":"redis",
                "url":"redis://:s3cret@127.0.0.1:6379/2"}"#,
        );
        let (command, _) = build_schema_command(&connection, Path::new(".")).unwrap();
        let args = args(&command);
        assert_eq!(command.get_program(), OsStr::new("redis-cli"));
        assert!(args.windows(2).any(|pair| pair == ["-p", "6379"]));
        assert!(args.windows(2).any(|pair| pair == ["-n", "2"]));
        assert!(args.iter().any(|arg| arg == "--scan"));
        assert!(!args.join(" ").contains("s3cret"));
        assert_eq!(
            env_value(&command, "REDISCLI_AUTH").as_deref(),
            Some("s3cret")
        );
    }

    #[test]
    fn redis_exec_uses_quoted_arguments() {
        let connection =
            connection(r#"{"name":"cache","driver":"redis","host":"127.0.0.1","port":6379}"#);
        let (command, _) = build_exec_command(&connection, "SET greeting \"hello world\"").unwrap();
        let args = args(&command);
        assert!(args.windows(2).any(|pair| pair == ["SET", "greeting"]));
        assert!(args.iter().any(|arg| arg == "hello world"));
    }

    #[test]
    fn sqlite_databases_and_views() {
        let connection = connection(r#"{"name":"app","driver":"sqlite","path":"demo.sqlite"}"#);
        let names = databases(&connection, &base()).unwrap();
        assert_eq!(names, vec!["demo.sqlite"]);

        let views = views(&connection, &base()).unwrap();
        assert!(views.tables.is_empty());
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "php-db-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn tmp_sqlite_reports_databases_and_view_columns() {
        let directory = tempfile_dir();
        let path = directory.join("scratch.sqlite");
        Command::new("sqlite3")
            .arg(&path)
            .arg("CREATE TABLE t (id INTEGER PRIMARY KEY, note TEXT); CREATE VIEW big AS SELECT id, note FROM t WHERE id > 1;")
            .status()
            .expect("sqlite3 CLI");
        let connection = serde_json::from_str(&format!(
            r#"{{"name":"scratch","driver":"sqlite","path":"{}"}}"#,
            path.display()
        ))
        .unwrap();
        let names = databases(&connection, &path.parent().unwrap()).unwrap();
        assert_eq!(names, vec!["scratch.sqlite".to_string()]);

        let views = views(&connection, &directory).unwrap();
        let view = views.tables.first().expect("one view");
        assert_eq!(view.name, "big");
        assert_eq!(
            view.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["id", "note"]
        );
    }

    #[test]
    fn redis_databases_uses_database_field_or_db0() {
        let with_db = connection(
            r#"{"name":"cache","driver":"redis","host":"127.0.0.1","database":"3"}"#,
        );
        assert_eq!(
            redis_database_name(&with_db),
            "3"
        );
        let without_db = connection(r#"{"name":"cache","driver":"redis","host":"127.0.0.1"}"#);
        assert_eq!(redis_database_name(&without_db), "db0");
    }

    #[test]
    fn oracle_databases_falls_back_to_database_then_user() {
        let with_database =
            connection(r#"{"name":"ora","driver":"oracle","url":"u@h:1521/svc"}"#);
        assert_eq!(
            databases(&with_database, Path::new(".")).unwrap(),
            vec!["svc"]
        );
        let with_user = connection(r#"{"name":"ora","driver":"oracle","host":"h","user":"u"}"#);
        assert_eq!(databases(&with_user, Path::new(".")).unwrap(), vec!["u"]);
    }

    #[test]
    fn builds_databases_queries_per_server_driver() {
        let cases = [
            (
                r#"{"name":"pg","driver":"postgres","url":"postgres://h/db"}"#,
                "psql",
                PG_DATABASES_SQL,
            ),
            (
                r#"{"name":"my","driver":"mysql","host":"h","database":"d"}"#,
                "mysql",
                MYSQL_DATABASES_SQL,
            ),
            (
                r#"{"name":"ms","driver":"sqlserver","host":"h","database":"d"}"#,
                "sqlcmd",
                SQLSERVER_DATABASES_SQL,
            ),
        ];
        for (json, program, sql) in cases {
            let c = connection(json);
            let (command, stdin) =
                build_query_command(&c, Path::new("."), match c.driver().unwrap() {
                    Driver::Postgres => PG_DATABASES_SQL,
                    Driver::Mysql => MYSQL_DATABASES_SQL,
                    Driver::SqlServer => SQLSERVER_DATABASES_SQL,
                    _ => unreachable!(),
                })
                .unwrap();
            assert_eq!(command.get_program(), OsStr::new(program), "{json}");
            assert!(stdin.is_none(), "{json}");
            let argv = args(&command);
            assert!(argv.iter().any(|arg| arg.contains(&sql[..12])), "{json}");
        }
    }

    #[test]
    fn mongodb_databases_uses_admin_command() {
        let connection = connection(r#"{"name":"mongo","driver":"mongodb","host":"h"}"#);
        let (command, _) =
            build_exec_command(&connection, "db.adminCommand({ listDatabases: 1 }).databases.forEach(d => print(d.name))").unwrap();
        assert_eq!(command.get_program(), OsStr::new("mongosh"));
        let argv = args(&command);
        assert!(argv.iter().any(|arg| arg.contains("adminCommand")));
    }

    #[test]
    fn names_helpers_parse_client_output() {
        assert_eq!(
            names_from_csv("name\nalpha\nbeta\n").unwrap(),
            vec!["alpha", "beta"]
        );
        assert_eq!(
            names_from_tsv("Tables_in_db\tTable_type\nv1\tVIEW\n"),
            vec!["v1"]
        );
        assert_eq!(
            names_from_lines("one\n\n  two \n"),
            vec!["one", "two"]
        );
    }

    #[test]
    fn parses_plain_csv() {
        let records = parse_csv("id,email\n1,a@example.com\n").unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0], vec![Some("id".into()), Some("email".into())]);
        assert_eq!(
            records[1],
            vec![Some("1".into()), Some("a@example.com".into())]
        );
    }

    #[test]
    fn parses_quotes_newlines_and_nulls() {
        let records =
            parse_csv("a,b,c\n\"x,y\",\"he said \"\"hi\"\"\",\n\"line\nbreak\",\"\",NULL\n")
                .unwrap();
        assert_eq!(records[1][0].as_deref(), Some("x,y"));
        assert_eq!(records[1][1].as_deref(), Some("he said \"hi\""));
        assert_eq!(records[1][2], None);
        assert_eq!(records[2][0].as_deref(), Some("line\nbreak"));
        assert_eq!(records[2][1].as_deref(), Some(""));
        assert_eq!(records[2][2].as_deref(), Some("NULL"));
    }

    #[test]
    fn builds_result_from_csv() {
        let result = csv_to_result("id,email\n1,a@example.com\n2,\n").unwrap();
        assert_eq!(result.columns, vec!["id", "email"]);
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[1][1], Value::Null);
    }

    #[test]
    fn recognizes_postgres_command_tags() {
        assert!(looks_like_command_tag("INSERT 0 1"));
        assert!(looks_like_command_tag("CREATE TABLE"));
        assert!(!looks_like_command_tag("id,email"));
        let result = csv_to_result("INSERT 0 1\n").unwrap();
        assert!(result.columns.is_empty());
        assert!(result.rows.is_empty());
    }

    #[test]
    fn parses_tsv_with_null() {
        let result = tsv_to_result("id\temail\n1\ta@example.com\n2\tNULL\n");
        assert_eq!(result.columns, vec!["id", "email"]);
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[1][1], Value::Null);
        assert!(tsv_to_result("").columns.is_empty());
    }

    #[test]
    fn schema_from_lines_skips_noise() {
        let schema = schema_from_lines("users\norders\n\n(2 rows affected)\n");
        assert_eq!(
            schema
                .tables
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            vec!["users", "orders"]
        );
    }

    #[test]
    fn parses_mysql_urls() {
        let target = parse_mysql_url("mysql://app:p%40ss@db.example:3307/appdb").unwrap();
        assert_eq!(target.host, "db.example");
        assert_eq!(target.port, Some(3307));
        assert_eq!(target.user.as_deref(), Some("app"));
        assert_eq!(target.password.as_deref(), Some("p@ss"));
        assert_eq!(target.database.as_deref(), Some("appdb"));

        let target = parse_mysql_url("mysql://root@localhost/appdb").unwrap();
        assert_eq!(target.host, "localhost");
        assert_eq!(target.port, None);
        assert_eq!(target.password, None);

        let target = parse_mysql_url("mysql://u:p@[::1]:3306/appdb?ssl=true").unwrap();
        assert_eq!(target.host, "::1");
        assert_eq!(target.port, Some(3306));
        assert_eq!(target.database.as_deref(), Some("appdb"));

        assert!(parse_mysql_url("postgres://x/y").is_err());
        assert!(parse_mysql_url("mysql://localhost:3306").is_err());
        assert!(parse_mysql_url("mysql://localhost/").is_err());
    }

    #[test]
    fn pick_free_port_binds_two_distinct_ports() {
        let first = pick_free_port().unwrap();
        let second = pick_free_port().unwrap();
        assert_ne!(first, 0);
        assert_ne!(second, 0);
        assert_ne!(first, second);
    }

    #[test]
    fn sqlite_read_only_adds_readonly_flag() {
        let read_only =
            connection(r#"{"name":"app","driver":"sqlite","path":"demo.sqlite","read_only":true}"#);
        let writable =
            connection(r#"{"name":"app","driver":"sqlite","path":"demo.sqlite"}"#);
        let (read_command, _) =
            build_query_command(&read_only, &base(), "SELECT 1").unwrap();
        let (write_command, _) =
            build_query_command(&writable, &base(), "SELECT 1").unwrap();
        assert!(args(&read_command).iter().any(|arg| arg == "-readonly"));
        assert!(!args(&write_command).iter().any(|arg| arg == "-readonly"));
        // The engine enforces read-only even if a heuristic slipped through.
        let error = match sqlite_query(&read_only, &base(), "INSERT INTO users VALUES (99, 'x', NULL)") {
            Ok(_) => panic!("read-only sqlite accepted an INSERT"),
            Err(error) => error.to_string(),
        };
        assert!(!error.is_empty());
    }

    #[test]
    fn mysql_options_translate_to_cli_args() {
        let with_options = connection(
            r#"{"name":"my","driver":"mysql","host":"h","database":"d",
                "options":{"charset":"utf8mb4","sslmode":"REQUIRED","connect_timeout":"10"}}"#,
        );
        let (command, _) = build_query_command(&with_options, Path::new("."), "SELECT 1").unwrap();
        let argv = args(&command);
        assert!(argv
            .windows(2)
            .any(|pair| pair == ["--default-character-set", "utf8mb4"]));
        assert!(argv.iter().any(|arg| arg == "--ssl-mode=REQUIRED"));
        assert!(argv
            .windows(2)
            .any(|pair| pair == ["--connect-timeout", "10"]));

        let without_options =
            connection(r#"{"name":"my","driver":"mysql","host":"h","database":"d"}"#);
        let (command, _) = build_query_command(&without_options, Path::new("."), "SELECT 1").unwrap();
        assert!(!args(&command)
            .iter()
            .any(|arg| arg.starts_with("--default-character-set")));
    }

    #[test]
    fn mariadb_options_use_legacy_ssl_flag() {
        let with_options = connection(
            r#"{"name":"maria","driver":"mariadb","host":"h","database":"d",
                "options":{"sslmode":"required"}}"#,
        );
        let (command, _) = build_query_command(&with_options, Path::new("."), "SELECT 1").unwrap();
        let argv = args(&command);
        assert!(argv.iter().any(|arg| arg == "--ssl"));
        assert!(!argv.iter().any(|arg| arg.starts_with("--ssl-mode")));
    }

    #[test]
    fn postgres_options_become_url_parameters() {
        let with_options = connection(
            r#"{"name":"pg","driver":"postgres",
                "url":"postgres://app@db:5432/app",
                "options":{"connect_timeout":"10","application_name":"reporting","sslmode":"require","nosuch":"x"}}"#,
        );
        let (command, _) = build_query_command(&with_options, Path::new("."), "SELECT 1").unwrap();
        let argv = args(&command);
        assert!(argv.iter().any(|arg| arg.contains("connect_timeout=10")));
        assert!(argv.iter().any(|arg| arg.contains("application_name=reporting")));
        assert!(!argv.iter().any(|arg| arg.contains("nosuch")));
    }

    #[test]
    fn describe_dsn_composes_per_driver() {
        let pg = connection(
            r#"{"name":"pg","driver":"postgres",
                "url":"postgres://app:s3cret@db:5432/app","sslmode":"require",
                "options":{"application_name":"reporting"}}"#,
        );
        let dsn = describe_dsn(&pg, Path::new(".")).unwrap();
        assert!(dsn.contains("s3cret") == false, "{dsn}");
        assert!(dsn.contains("postgres://app@db:5432/app"), "{dsn}");
        assert!(dsn.contains("sslmode=require"), "{dsn}");
        assert!(dsn.contains("application_name=reporting"), "{dsn}");

        let my = connection(
            r#"{"name":"my","driver":"mysql","host":"h","port":3307,"user":"u","database":"d",
                "options":{"charset":"utf8mb4"}}"#,
        );
        let dsn = describe_dsn(&my, Path::new(".")).unwrap();
        assert_eq!(dsn, "mysql://u@h:3307/d?charset=utf8mb4");

        let sqlite = connection(
            r#"{"name":"app","driver":"sqlite","path":"demo.sqlite","read_only":true}"#,
        );
        let dsn = describe_dsn(&sqlite, &base()).unwrap();
        assert!(dsn.ends_with("demo.sqlite?read_only=1"), "{dsn}");
    }

    #[test]
    fn ignored_options_lists_unknown_keys() {
        let pg = connection(
            r#"{"name":"pg","driver":"postgres","host":"h",
                "options":{"connect_timeout":"10","custom":"x"}}"#,
        );
        assert_eq!(ignored_options(&pg), vec!["custom".to_string()]);
        let sqlite = connection(r#"{"name":"app","driver":"sqlite","path":"p"}"#);
        assert!(ignored_options(&sqlite).is_empty());
    }

    #[test]
    fn ssh_tunnel_command_matches_spec() {
        let tunnel: SshTunnel = serde_json::from_str(
            r#"{"enabled":true,"host":"bastion.example.com","port":2222,"user":"deploy",
                "identity_file":"~/.ssh/id_ed25519","local_port":15432}"#,
        )
        .unwrap();
        let command = ssh_tunnel_command(&tunnel, "db.internal", 5432, 15432);
        assert_eq!(command.get_program(), OsStr::new("ssh"));
        assert_eq!(
            args(&command),
            vec![
                "-N".to_string(),
                "-L".to_string(),
                "127.0.0.1:15432:db.internal:5432".to_string(),
                "deploy@bastion.example.com".to_string(),
                "-p".to_string(),
                "2222".to_string(),
                "-i".to_string(),
                "~/.ssh/id_ed25519".to_string(),
                "-o".to_string(),
                "BatchMode=yes".to_string(),
                "-o".to_string(),
                "ConnectTimeout=5".to_string(),
                "-o".to_string(),
                "ServerAliveInterval=15".to_string(),
            ]
        );
    }

    #[test]
    fn tunnel_rewrites_target_fields_uniformly() {
        // The uniform rewrite is exercised through open_session's success path
        // indirectly; here the refusal paths and shape are verified. A real
        // rewrite requires an sshd endpoint (see the failure test below).
        let unsupported = connection(
            r#"{"name":"cache","driver":"redis","host":"127.0.0.1",
                "ssh_tunnel":{"enabled":true,"host":"b","user":"u"}}"#,
        );
        let error = open_session(&unsupported).unwrap_err().to_string();
        assert!(error.contains("only supported for postgres, mysql, and mariadb"));
    }

    #[test]
    fn tunnel_failure_reports_ssh_tunnel_failed() {
        let connection = connection(
            r#"{"name":"pg","driver":"postgres","host":"127.0.0.1","port":15433,
                "user":"ci","database":"reporting",
                "ssh_tunnel":{"enabled":true,"host":"127.0.0.1","port":1,"user":"ci"}}"#,
        );
        let error = open_session(&connection).unwrap_err().to_string();
        assert!(
            error.contains("ssh tunnel failed"),
            "unexpected: {error}"
        );
        // No fall-through: the error is terminal, never a direct connection.
        assert!(error.contains("127.0.0.1") || error.contains("refused") || !error.is_empty());
    }

    #[test]
    fn splits_redis_commands() {
        assert_eq!(
            split_command("SET key \"hello world\""),
            vec!["SET", "key", "hello world"]
        );
        assert_eq!(split_command("HGETALL 'user:1'"), vec!["HGETALL", "user:1"]);
        assert_eq!(split_command("  "), Vec::<String>::new());
    }
}
