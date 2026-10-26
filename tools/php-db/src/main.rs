mod config;
mod db;
mod guard;

use anyhow::{bail, Context, Result};
use config::Store;
use serde_json::{json, Value};
use std::fmt;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
php-db - database engine for a PhpStorm-like IDE

Usage:
  php-db [--config PATH] list [--json]
  php-db [--config PATH] check <connection> [--json]
  php-db [--config PATH] schema <connection> [--json]
  php-db [--config PATH] query <connection> \"<sql>\" [--json] [--allow-write]
  php-db [--config PATH] exec <connection> \"<command>\" [--allow-write]

Options:
  --config PATH   use this .zed/databases.json instead of walking up from the current directory
  --json          print JSON instead of a table
  --allow-write   permit destructive statements and exec commands
  -h, --help      show this help
  -V, --version   show the version

Destructive statements (drop, truncate, delete) are refused with exit code 2
unless --allow-write is given. `exec` is a write-capable escape hatch and
always requires --allow-write.
";

#[derive(Default)]
struct Cli {
    config: Option<PathBuf>,
    json: bool,
    allow_write: bool,
    help: bool,
    version: bool,
    rest: Vec<String>,
}

#[derive(Debug)]
struct Refused(String);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("php-db: {error:#}");
            if error.downcast_ref::<Refused>().is_some() {
                ExitCode::from(2)
            } else {
                ExitCode::from(1)
            }
        }
    }
}

fn run() -> Result<()> {
    let cli = parse_args(std::env::args().skip(1).collect())?;
    if cli.help {
        print!("{USAGE}");
        return Ok(());
    }
    if cli.version {
        println!("php-db {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let Some(command) = cli.rest.first().map(String::as_str) else {
        bail!("no command given\n\n{USAGE}");
    };
    let args = &cli.rest[1..];

    match command {
        "list" => {
            if !args.is_empty() {
                bail!("`list` takes no arguments");
            }
            let store = Store::load(cli.config.as_deref())?;
            cmd_list(&store, cli.json)
        }
        "check" => {
            let [name] = args else {
                bail!("usage: php-db check <connection> [--json]");
            };
            let store = Store::load(cli.config.as_deref())?;
            cmd_check(&store, name, cli.json)
        }
        "schema" => {
            let [name] = args else {
                bail!("usage: php-db schema <connection> [--json]");
            };
            let store = Store::load(cli.config.as_deref())?;
            cmd_schema(&store, name, cli.json)
        }
        "query" => {
            let [name, sql] = args else {
                if args.len() > 2 {
                    bail!("the SQL must be a single argument (quote it): php-db query <connection> \"<sql>\"");
                }
                bail!("usage: php-db query <connection> \"<sql>\" [--json] [--allow-write]");
            };
            let store = Store::load(cli.config.as_deref())?;
            let connection = store.get(name)?;
            let driver = connection.driver()?;
            if !driver.is_sql() {
                bail!(
                    "`query` is only available for SQL drivers; use `php-db exec {name} \"<command>\" --allow-write` for {driver}"
                );
            }

            if !cli.allow_write {
                if let Some(keyword) = guard::destructive_keyword(sql) {
                    return Err(Refused(format!(
                        "refusing to run a `{keyword}` statement without --allow-write"
                    ))
                    .into());
                }
            }

            let result = db::query(connection, &store.base, sql)?;
            print_query(&result, cli.json);
            Ok(())
        }
        "exec" => {
            let [name, command] = args else {
                bail!("usage: php-db exec <connection> \"<command>\" [--allow-write]");
            };
            let store = Store::load(cli.config.as_deref())?;
            let connection = store.get(name)?;
            let driver = connection.driver()?;
            if driver.is_sql() {
                bail!("`exec` is only available for mongodb and redis; use `query` for {driver}");
            }
            if !cli.allow_write {
                return Err(
                    Refused("refusing to run `exec` without --allow-write".to_string()).into(),
                );
            }
            let output = db::exec(connection, command)?;
            print!("{output}");
            if !output.ends_with('\n') {
                println!();
            }
            Ok(())
        }
        "help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => bail!("unknown command `{other}`\n\n{USAGE}"),
    }
}

fn parse_args(argv: Vec<String>) -> Result<Cli> {
    let mut cli = Cli::default();
    let mut args = argv.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--config" {
            let value = args.next().context("--config requires a path argument")?;
            cli.config = Some(PathBuf::from(value));
        } else if let Some(value) = arg.strip_prefix("--config=") {
            cli.config = Some(PathBuf::from(value));
        } else if arg == "--json" {
            cli.json = true;
        } else if arg == "--allow-write" {
            cli.allow_write = true;
        } else if arg == "-h" || arg == "--help" {
            cli.help = true;
        } else if arg == "-V" || arg == "--version" {
            cli.version = true;
        } else if arg.starts_with('-') && arg != "-" {
            bail!("unknown option `{arg}`");
        } else {
            cli.rest.push(arg);
        }
    }
    Ok(cli)
}

fn cmd_list(store: &Store, json_output: bool) -> Result<()> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut entries: Vec<Value> = Vec::new();

    for connection in &store.connections {
        let driver = connection.driver()?;
        let client = driver.client();
        let client_available = db::client_available(client);
        let target = connection.target();
        let warnings = connection.warnings();

        let (status, reachable) = match driver {
            config::Driver::Sqlite => {
                let path = connection.sqlite_path(&store.base)?;
                let exists = path.is_file();
                (
                    if exists {
                        "file exists".to_string()
                    } else {
                        format!("file missing ({})", path.display())
                    },
                    json!(exists),
                )
            }
            _ => (
                if client_available {
                    "configured".to_string()
                } else {
                    "configured (client missing)".to_string()
                },
                Value::Null,
            ),
        };

        let mut status_text = status.clone();
        for warning in &warnings {
            status_text.push_str(&format!("; WARNING: {warning}"));
        }
        rows.push(vec![
            connection.name.clone(),
            connection.driver.clone(),
            target.clone(),
            format!(
                "{client} ({})",
                if client_available { "found" } else { "missing" }
            ),
            status_text,
        ]);

        entries.push(json!({
            "name": connection.name,
            "driver": connection.driver,
            "target": target,
            "client": client,
            "client_available": client_available,
            "status": status,
            "reachable": reachable,
            "warnings": warnings,
        }));
    }

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "connections": entries }))?
        );
    } else {
        let headers = vec![
            "NAME".to_string(),
            "DRIVER".to_string(),
            "TARGET".to_string(),
            "CLIENT".to_string(),
            "STATUS".to_string(),
        ];
        print_table(&headers, &rows);
    }
    Ok(())
}

fn cmd_check(store: &Store, name: &str, json_output: bool) -> Result<()> {
    let connection = store.get(name)?;
    let driver = connection.driver()?;
    let message = db::check(connection, &store.base)?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "connection": connection.name,
                "driver": driver.as_str(),
                "target": connection.target(),
                "ok": true,
                "message": message,
            }))?
        );
    } else {
        println!("{} ({}): ok ({})", connection.name, driver, message);
    }
    Ok(())
}

fn cmd_schema(store: &Store, name: &str, json_output: bool) -> Result<()> {
    let connection = store.get(name)?;
    let schema = db::schema(connection, &store.base)?;

    if json_output {
        let tables: Vec<Value> = schema
            .tables
            .iter()
            .map(|table| {
                json!({
                    "name": table.name,
                    "columns": table.columns.iter().map(|column| json!({
                        "name": column.name,
                        "type": column.kind,
                        "not_null": column.not_null,
                        "primary_key": column.primary_key,
                        "default": column.default,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "connection": connection.name,
                "driver": connection.driver,
                "tables": tables,
            }))?
        );
        return Ok(());
    }

    println!("{} ({})", connection.name, connection.driver);
    if schema.tables.is_empty() {
        println!("  (no tables)");
        return Ok(());
    }
    for table in &schema.tables {
        println!("  {}", table.name);
        let width = table
            .columns
            .iter()
            .map(|column| column.name.chars().count())
            .max()
            .unwrap_or(0);
        for column in &table.columns {
            let kind = if column.kind.is_empty() {
                "-"
            } else {
                column.kind.as_str()
            };
            let mut flags: Vec<String> = Vec::new();
            if column.primary_key {
                flags.push("PK".to_string());
            }
            if column.not_null {
                flags.push("NOT NULL".to_string());
            }
            if let Some(default) = &column.default {
                flags.push(format!("DEFAULT {default}"));
            }
            let suffix = if flags.is_empty() {
                String::new()
            } else {
                format!("  [{}]", flags.join(", "))
            };
            println!("    {:width$}  {kind}{suffix}", column.name, width = width);
        }
    }
    Ok(())
}

fn print_query(result: &db::QueryResult, json_output: bool) {
    if json_output {
        match serde_json::to_string_pretty(&result.to_json()) {
            Ok(text) => println!("{text}"),
            Err(error) => eprintln!("php-db: could not serialize result: {error}"),
        }
        return;
    }

    if result.columns.is_empty() {
        println!("no rows returned");
        return;
    }

    let headers = result.columns.clone();
    let rows: Vec<Vec<String>> = result
        .rows
        .iter()
        .map(|row| row.iter().map(cell_text).collect())
        .collect();
    print_table(&headers, &rows);
    println!("({} rows)", result.rows.len());
}

fn cell_text(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_string(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn print_table(headers: &[String], rows: &[Vec<String>]) {
    let mut widths: Vec<usize> = headers
        .iter()
        .map(|header| header.chars().count())
        .collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            if let Some(width) = widths.get_mut(index) {
                *width = (*width).max(cell.chars().count());
            }
        }
    }

    let render = |cells: &[String]| -> String {
        let mut line = String::new();
        for (index, cell) in cells.iter().enumerate() {
            if index > 0 {
                line.push_str(" | ");
            }
            line.push_str(cell);
            let padding = widths
                .get(index)
                .copied()
                .unwrap_or(0)
                .saturating_sub(cell.chars().count());
            for _ in 0..padding {
                line.push(' ');
            }
        }
        line.trim_end().to_string()
    };

    println!("{}", render(headers));
    let separator = widths
        .iter()
        .map(|width| "-".repeat((*width).max(1)))
        .collect::<Vec<_>>()
        .join("-+-");
    println!("{separator}");
    for row in rows {
        println!("{}", render(row));
    }
}
