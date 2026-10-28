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
fn databases_and_views_end_to_end_sqlite() {
    let output = run(&["databases", "app", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let value: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(value["connection"], "app");
    assert_eq!(value["driver"], "sqlite");
    assert_eq!(value["databases"], serde_json::json!(["demo.sqlite"]));

    let output = run(&["views", "app", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let value: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(value["driver"], "sqlite");
    // The fixture has no views; the shape is still the schema row shape.
    assert_eq!(value["views"], serde_json::json!([]));

    let output = run(&["views", "app"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("(no views)"));
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

// ---------------------------------------------------------------------------
// read_only, ssh tunnels, --verbose-dsn
// ---------------------------------------------------------------------------

fn temp_project(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "php-db-cli-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(dir.join(".zed")).unwrap();
    dir
}

#[test]
fn read_only_connection_refuses_writes_and_blocks_exec() {
    let project = temp_project("ro");
    let path = project.join("demo.sqlite");
    let status = Command::new("sqlite3")
        .arg(&path)
        .arg("CREATE TABLE t (id INTEGER)")
        .status()
        .expect("sqlite3 CLI");
    assert!(status.success());
    let config = project.join(".zed").join("databases.json");
    std::fs::write(
        &config,
        r#"{"connections":[
        {"name":"ro","driver":"sqlite","path":"demo.sqlite","read_only":true},
        {"name":"rw","driver":"sqlite","path":"demo.sqlite"}
    ]}"#,
    )
    .unwrap();
    let config = config.to_str().unwrap();

    // A write keyword is refused with exit code 2 and a read_only message,
    // even with --allow-write.
    let output = Command::new(binary())
        .args(["--config", config, "query", "ro", "INSERT INTO t VALUES (1)"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("is read_only"), "{:?}", String::from_utf8_lossy(&output.stderr));

    let output = Command::new(binary())
        .args([
            "--config",
            config,
            "query",
            "ro",
            "insert into t values (1)",
            "--allow-write",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));

    // Reads are fine.
    let output = Command::new(binary())
        .args(["--config", config, "query", "ro", "SELECT 1", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));

    // The same statement is also fine without read_only (sqlite checks only
    // the destructive keywords here, and CREATE-free benign writes pass).
    let output = Command::new(binary())
        .args(["--config", config, "query", "rw", "SELECT 1"])
        .output()
        .unwrap();
    assert!(output.status.success());
}

#[test]
fn tunnel_failure_is_reported_without_fallthrough() {
    let output = Command::new(binary())
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/db-test"))
        .args(["check", "pg-tunnel-dead"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("ssh tunnel failed"), "{error}");
    // The dead tunnel must not fall through to a direct connection: the
    // message is the tunnel failure itself, not a psql error.
    assert!(!error.contains("psql failed"), "{error}");
}

#[test]
fn verbose_dsn_prints_composed_dsn_and_ignored_options() {
    let project = temp_project("dsn");
    let path = project.join("demo.sqlite");
    let status = Command::new("sqlite3")
        .arg(&path)
        .arg("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .status()
        .expect("sqlite3 CLI");
    assert!(status.success());
    std::fs::write(
        project.join(".zed").join("databases.json"),
        format!(
            r#"{{"connections":[{{"name":"app","driver":"sqlite","path":"{}","read_only":true,
            "options":{{"unknown_for_sqlite":"x"}}}}]}}"#,
            path.display()
        ),
    )
    .unwrap();
    let config = project.join(".zed").join("databases.json");
    let config = config.to_str().unwrap();

    let output = Command::new(binary())
        .args(["--config", config, "--verbose-dsn", "app"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.starts_with("DSN: sqlite3:"), "{text}");
    assert!(text.contains("?read_only=1"), "{text}");
    assert!(text.contains("ignored-options: unknown_for_sqlite"), "{text}");

    // The flag also annotates a normal run (stderr) without changing it.
    let output = Command::new(binary())
        .args(["--config", config, "--verbose-dsn", "query", "app", "SELECT 1"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("1"));
    let error = stderr(&output);
    assert!(error.contains("DSN:"), "{error}");
}
