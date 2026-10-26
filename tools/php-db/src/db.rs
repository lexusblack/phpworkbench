//! Database access by shelling out to the platform CLIs (`sqlite3`, `psql`,
//! `mysql`). No database driver crates are linked.

use crate::config::{percent_decode, Connection};
use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Output};

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

pub fn client_available(bin: &str) -> bool {
    match Command::new(bin).arg("--version").output() {
        Ok(_) => true,
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

fn require_client(driver: &str, bin: &str) -> Result<()> {
    if !client_available(bin) {
        bail!("{driver} support requires the `{bin}` CLI on PATH, but it was not found");
    }
    Ok(())
}

pub fn schema(connection: &Connection, base: &Path) -> Result<Schema> {
    match connection.driver.as_str() {
        "sqlite" => sqlite_schema(connection, base),
        "postgres" => pg_schema(connection),
        "mysql" => mysql_schema(connection),
        other => bail!("unsupported driver `{other}`"),
    }
}

pub fn query(connection: &Connection, base: &Path, sql: &str) -> Result<QueryResult> {
    match connection.driver.as_str() {
        "sqlite" => sqlite_query(connection, base, sql),
        "postgres" => pg_query(connection, sql),
        "mysql" => mysql_query(connection, sql),
        other => bail!("unsupported driver `{other}`"),
    }
}

fn run(mut command: Command, label: &str) -> Result<Output> {
    let output = command.output().with_context(|| {
        format!("failed to run `{label}`; is the client installed and on PATH?")
    })?;
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

fn sqlite_database(connection: &Connection, base: &Path) -> Result<std::path::PathBuf> {
    let path = connection.sqlite_path(base)?;
    if !path.is_file() {
        bail!(
            "sqlite database not found: {} (php-db never creates a database)",
            path.display()
        );
    }
    Ok(path)
}

fn sqlite_schema(connection: &Connection, base: &Path) -> Result<Schema> {
    let path = sqlite_database(connection, base)?;
    let table_rows = sqlite_rows(
        &path,
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;

    let mut tables = Vec::with_capacity(table_rows.len());
    for row in table_rows {
        let name = string_field(&row, "name");
        if name.is_empty() {
            continue;
        }
        let pragma = format!("PRAGMA table_info(\"{}\")", name.replace('"', "\"\""));
        let columns = sqlite_rows(&path, &pragma)?
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
    let path = sqlite_database(connection, base)?;
    let rows = sqlite_rows(&path, sql)?;
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

fn sqlite_rows(path: &Path, sql: &str) -> Result<Vec<Map<String, Value>>> {
    let mut command = Command::new("sqlite3");
    command.arg("-json").arg(path).arg(sql);
    let output = run(command, "sqlite3")?;
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
SELECT c.table_schema, c.table_name, c.column_name, c.data_type, c.is_nullable, c.column_default,
       EXISTS (
           SELECT 1
           FROM information_schema.table_constraints tc
           JOIN information_schema.key_column_usage kcu
             ON kcu.constraint_name = tc.constraint_name
            AND kcu.table_schema = tc.table_schema
            AND kcu.table_name = tc.table_name
           WHERE tc.constraint_type = 'PRIMARY KEY'
             AND kcu.table_schema = c.table_schema
             AND kcu.table_name = c.table_name
             AND kcu.column_name = c.column_name
       ) AS is_pk
FROM information_schema.columns c
JOIN information_schema.tables t
  ON t.table_schema = c.table_schema
 AND t.table_name = c.table_name
WHERE t.table_type = 'BASE TABLE'
  AND c.table_schema NOT IN ('pg_catalog', 'information_schema')
ORDER BY c.table_schema, c.table_name, c.ordinal_position";

fn pg_command(connection: &Connection, sql: &str) -> Result<Command> {
    let url = connection
        .url
        .as_deref()
        .context("postgres connection is missing `url`")?;
    let mut command = Command::new("psql");
    command
        .arg(url)
        .arg("-X")
        .arg("--csv")
        .arg("-v")
        .arg("ON_ERROR_STOP=1")
        .arg("-c")
        .arg(sql);
    if let Some(password) = connection.password()? {
        command.env("PGPASSWORD", password);
    }
    Ok(command)
}

fn pg_schema(connection: &Connection) -> Result<Schema> {
    require_client("postgres", "psql")?;
    let output = run(pg_command(connection, PG_SCHEMA_SQL)?, "psql")?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let records = parse_csv(&text)?;
    if records.is_empty() {
        return Ok(Schema { tables: Vec::new() });
    }

    let mut tables: Vec<Table> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for record in records.iter().skip(1) {
        let schema_name = field(record, 0);
        let table_name = field(record, 1);
        if table_name.is_empty() {
            continue;
        }
        let key = format!("{schema_name}\u{1f}{table_name}");
        let table_index = *index.entry(key).or_insert_with(|| {
            tables.push(Table {
                name: if schema_name == "public" {
                    table_name.to_string()
                } else {
                    format!("{schema_name}.{table_name}")
                },
                columns: Vec::new(),
            });
            tables.len() - 1
        });
        let nullable = field(record, 4);
        tables[table_index].columns.push(Column {
            name: field(record, 2).to_string(),
            kind: field(record, 3).to_string(),
            not_null: nullable.eq_ignore_ascii_case("NO"),
            primary_key: matches!(field(record, 6), "t" | "true" | "1"),
            default: record.get(5).cloned().flatten(),
        });
    }
    Ok(Schema { tables })
}

fn pg_query(connection: &Connection, sql: &str) -> Result<QueryResult> {
    require_client("postgres", "psql")?;
    let output = run(pg_command(connection, sql)?, "psql")?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    csv_to_result(&text)
}

fn field(record: &[Option<String>], index: usize) -> &str {
    record
        .get(index)
        .and_then(|value| value.as_deref())
        .unwrap_or("")
}

// ---------------------------------------------------------------------------
// MySQL
// ---------------------------------------------------------------------------

const MYSQL_SCHEMA_SQL: &str = "\
SELECT c.table_name, c.column_name, c.column_type, c.is_nullable, c.column_default,
       (c.column_key = 'PRI') AS is_pk
FROM information_schema.columns c
JOIN information_schema.tables t
  ON t.table_schema = c.table_schema
 AND t.table_name = c.table_name
WHERE c.table_schema = DATABASE()
  AND t.table_type = 'BASE TABLE'
ORDER BY c.table_name, c.ordinal_position";

struct MysqlTarget {
    host: String,
    port: Option<u16>,
    user: Option<String>,
    password: Option<String>,
    database: String,
}

fn parse_mysql_url(url: &str) -> Result<MysqlTarget> {
    let rest = url
        .strip_prefix("mysql://")
        .with_context(|| format!("mysql url must start with mysql://: {url}"))?;
    let (authority, database_part) = rest
        .split_once('/')
        .context("mysql url must include a database name")?;
    let database = database_part
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .to_string();
    if database.is_empty() {
        bail!("mysql url must include a database name");
    }

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
        bail!("mysql url has an empty user name");
    }
    let (host, port) = split_host_port(host_port)?;
    if host.is_empty() {
        bail!("mysql url is missing a host");
    }
    Ok(MysqlTarget {
        host,
        port,
        user,
        password,
        database,
    })
}

fn split_host_port(value: &str) -> Result<(String, Option<u16>)> {
    if let Some(rest) = value.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .context("mysql url has an unterminated IPv6 host")?;
        let port = match tail.strip_prefix(':') {
            Some(port) => Some(port.parse().context("mysql url has an invalid port")?),
            None => None,
        };
        return Ok((host.to_string(), port));
    }
    match value.rsplit_once(':') {
        Some((host, port)) => Ok((
            host.to_string(),
            Some(port.parse().context("mysql url has an invalid port")?),
        )),
        None => Ok((value.to_string(), None)),
    }
}

fn mysql_command(connection: &Connection, sql: &str) -> Result<Command> {
    let url = connection
        .url
        .as_deref()
        .context("mysql connection is missing `url`")?;
    let target = parse_mysql_url(url)?;

    let mut command = Command::new("mysql");
    command
        .arg("--batch")
        .arg("--raw")
        .arg("-h")
        .arg(&target.host);
    if let Some(port) = target.port {
        command.arg("-P").arg(port.to_string());
    }
    if let Some(user) = &target.user {
        command.arg("-u").arg(user);
    }
    command.arg("-D").arg(&target.database);

    // MYSQL_PWD keeps the password out of the process arguments.
    let password = match connection.password()? {
        Some(password) => Some(password),
        None => target.password,
    };
    if let Some(password) = password {
        command.env("MYSQL_PWD", password);
    }
    command.arg("-e").arg(sql);
    Ok(command)
}

fn mysql_schema(connection: &Connection) -> Result<Schema> {
    require_client("mysql", "mysql")?;
    let output = run(mysql_command(connection, MYSQL_SCHEMA_SQL)?, "mysql")?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let result = tsv_to_result(&text);

    let mut tables: Vec<Table> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for row in &result.rows {
        let table_name = cell(row, 0);
        if table_name.is_empty() {
            continue;
        }
        let table_index = *index.entry(table_name.clone()).or_insert_with(|| {
            tables.push(Table {
                name: table_name.clone(),
                columns: Vec::new(),
            });
            tables.len() - 1
        });
        let nullable = cell(row, 3);
        tables[table_index].columns.push(Column {
            name: cell(row, 1),
            kind: cell(row, 2),
            not_null: nullable.eq_ignore_ascii_case("NO"),
            primary_key: cell(row, 5) == "1",
            default: cell_opt(row, 4),
        });
    }
    Ok(Schema { tables })
}

fn mysql_query(connection: &Connection, sql: &str) -> Result<QueryResult> {
    require_client("mysql", "mysql")?;
    let output = run(mysql_command(connection, sql)?, "mysql")?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok(tsv_to_result(&text))
}

// ---------------------------------------------------------------------------
// Output parsing
// ---------------------------------------------------------------------------

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

fn cell(row: &[Value], index: usize) -> String {
    match row.get(index) {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn cell_opt(row: &[Value], index: usize) -> Option<String> {
    match row.get(index) {
        Some(Value::String(value)) => Some(value.clone()),
        Some(Value::Null) | None => None,
        Some(other) => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn parses_mysql_urls() {
        let target = parse_mysql_url("mysql://app:p%40ss@db.example:3307/appdb").unwrap();
        assert_eq!(target.host, "db.example");
        assert_eq!(target.port, Some(3307));
        assert_eq!(target.user.as_deref(), Some("app"));
        assert_eq!(target.password.as_deref(), Some("p@ss"));
        assert_eq!(target.database, "appdb");

        let target = parse_mysql_url("mysql://root@localhost/appdb").unwrap();
        assert_eq!(target.host, "localhost");
        assert_eq!(target.port, None);
        assert_eq!(target.password, None);

        let target = parse_mysql_url("mysql://u:p@[::1]:3306/appdb?ssl=true").unwrap();
        assert_eq!(target.host, "::1");
        assert_eq!(target.port, Some(3306));
        assert_eq!(target.database, "appdb");

        assert!(parse_mysql_url("postgres://x/y").is_err());
        assert!(parse_mysql_url("mysql://localhost:3306").is_err());
        assert!(parse_mysql_url("mysql://localhost/").is_err());
    }
}
