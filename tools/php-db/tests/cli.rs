use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_php-db")
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/db-drivers")
}

fn run(args: &[&str]) -> Output {
    Command::new(binary())
        .current_dir(fixture())
        .args(args)
        .output()
        .expect("php-db should start")
}

fn run_without_clients(args: &[&str]) -> Output {
    Command::new(binary())
        .current_dir(fixture())
        .env("PATH", "/nonexistent-php-db-path")
        .args(args)
        .output()
        .expect("php-db should start")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn list_reports_all_drivers_and_redacted_targets() {
    let output = run(&["list", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let value: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    let connections = value["connections"].as_array().unwrap();
    assert_eq!(connections.len(), 8);

    let names = connections
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec!["app", "pg", "my", "maria", "ms", "ora", "mongo", "cache"]
    );

    let pg = &connections[1];
    assert_eq!(pg["target"], "postgres://user@host:5432/db");
    assert_eq!(
        pg["warnings"][0],
        "ssh_tunnel is configured but not active yet"
    );

    for entry in connections {
        let target = entry["target"].as_str().unwrap();
        assert!(!target.contains("password"), "{target}");
        assert!(!target.contains("s3cret"), "{target}");
    }
}

#[test]
fn sqlite_end_to_end() {
    let output = run(&["check", "app"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("ok (file exists)"));

    let output = run(&["schema", "app"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("users"));
    assert!(text.contains("email"));
    assert!(text.contains("[NOT NULL]"));

    let output = run(&[
        "query",
        "app",
        "select id, email from users order by id",
        "--json",
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    let value: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(value.as_array().unwrap().len(), 3);
    assert_eq!(value[0]["email"], "alice@example.com");

    let output = run(&["query", "app", "select id from users order by id"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("(3 rows)"));
}

#[test]
fn destructive_query_needs_allow_write() {
    let output = run(&["query", "app", "delete from users"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("refusing to run a `delete` statement"));
}

#[test]
fn exec_requires_allow_write() {
    let output = run(&["exec", "cache", "GET key"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("without --allow-write"));
}

#[test]
fn query_is_only_for_sql_drivers() {
    let output = run(&["query", "cache", "select 1"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("only available for SQL drivers"));
}

#[test]
fn missing_psql_fails_cleanly() {
    let output = run_without_clients(&["check", "pg"]);
    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(error.contains("`psql`"), "{error}");
    assert!(error.contains("not found"), "{error}");
}

#[test]
fn missing_redis_cli_fails_cleanly() {
    let output = run_without_clients(&["check", "cache"]);
    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(error.contains("`redis-cli`"), "{error}");
    assert!(error.contains("not found"), "{error}");
}

#[test]
fn every_missing_client_is_named() {
    let cases = [
        ("check", "app", "sqlite3"),
        ("check", "pg", "psql"),
        ("check", "my", "mysql"),
        ("check", "ms", "sqlcmd"),
        ("check", "ora", "sqlplus"),
        ("check", "mongo", "mongosh"),
        ("check", "cache", "redis-cli"),
    ];
    for (command, name, client) in cases {
        let output = run_without_clients(&[command, name]);
        assert_eq!(output.status.code(), Some(1), "{name}");
        let error = stderr(&output);
        assert!(error.contains(client), "{name}: {error}");
    }
}
