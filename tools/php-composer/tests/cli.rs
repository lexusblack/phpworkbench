#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "php-composer-cli-{name}-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fake_composer(dir: &Path) -> PathBuf {
    let script = dir.join("fake-composer");
    fs::write(
        &script,
        "#!/bin/sh\n\
         printf '%s\\n' \"$@\" >> \"$FAKE_ARGS\"\n\
         cmd=\"$1\"\n\
         case \"$cmd\" in\n\
           outdated) printf '%s' \"$FAKE_OUTDATED\" ;;\n\
           validate) printf '%s' \"$FAKE_VALIDATE_STDOUT\"; printf '%s' \"$FAKE_VALIDATE_STDERR\" >&2 ;;\n\
           show) printf '%s' \"$FAKE_SHOW\" ;;\n\
           run-script) printf 'script-out\\n'; printf 'script-err\\n' >&2 ;;\n\
           require|remove) printf '%s-dry\\n' \"$cmd\" ;;\n\
           *) printf 'unknown fake command: %s\\n' \"$cmd\" >&2; exit 3 ;;\n\
         esac\n\
         exit \"${FAKE_EXIT:-0}\"\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).unwrap();
    script
}

fn write_project(dir: &Path) {
    fs::write(
        dir.join("composer.json"),
        r#"{
  "name": "fixture/test",
  "require": {
    "php": "^8.0",
    "psr/log": "^3",
    "guzzlehttp/guzzle": "^7"
  },
  "require-dev": {
    "phpunit/phpunit": "^11"
  },
  "scripts": {
    "test": "@php -r \"echo 1;\"",
    "all": ["@test", "echo done"]
  }
}"#,
    )
    .unwrap();
    fs::write(
        dir.join("composer.lock"),
        r#"{
  "packages": [
    {"name": "psr/log", "version": "3.0.2"},
    {"name": "guzzlehttp/guzzle", "version": "7.10.0"}
  ],
  "packages-dev": [
    {"name": "phpunit/phpunit", "version": "11.5.0"}
  ]
}"#,
    )
    .unwrap();
}

fn run(dir: &Path, composer: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let args_file = dir.join("args.txt");
    let mut command = Command::new(env!("CARGO_BIN_EXE_php-composer"));
    command
        .current_dir(dir)
        .args(args)
        .env("PHP_COMPOSER_BIN", composer)
        .env("FAKE_ARGS", &args_file);
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().unwrap()
}

fn args_file(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("args.txt"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn list_outputs_direct_packages_with_lock_versions() {
    let dir = TempDir::new("list");
    write_project(dir.path());
    let composer = fake_composer(dir.path());

    let output = run(dir.path(), &composer, &["list", "--json"], &[]);
    assert!(output.status.success());
    assert_eq!(
        json(&output),
        serde_json::json!([
            {"name":"guzzlehttp/guzzle","version":"7.10.0","constraint":"^7","dev":false},
            {"name":"phpunit/phpunit","version":"11.5.0","constraint":"^11","dev":true},
            {"name":"psr/log","version":"3.0.2","constraint":"^3","dev":false}
        ])
    );
    assert!(args_file(dir.path()).is_empty());
}

#[test]
fn scripts_outputs_names_and_commands() {
    let dir = TempDir::new("scripts");
    write_project(dir.path());
    let composer = fake_composer(dir.path());

    let output = run(dir.path(), &composer, &["scripts", "--json"], &[]);
    assert!(output.status.success());
    assert_eq!(
        json(&output),
        serde_json::json!([
            {"name":"all","commands":["@test","echo done"]},
            {"name":"test","commands":["@php -r \"echo 1;\""]}
        ])
    );
}

#[test]
fn outdated_normalizes_and_uses_direct_json_format() {
    let dir = TempDir::new("outdated");
    write_project(dir.path());
    let composer = fake_composer(dir.path());

    let output = run(
        dir.path(),
        &composer,
        &["outdated", "--json"],
        &[(
            "FAKE_OUTDATED",
            r#"[{"name":"psr/log","version":"3.0.1","latest":"3.0.2"},{"name":"guzzlehttp/guzzle","version":"7.9.0","latest":"7.10.0"}]"#,
        )],
    );
    assert!(output.status.success());
    assert_eq!(
        json(&output),
        serde_json::json!([
            {"name":"guzzlehttp/guzzle","installed":"7.9.0","latest":"7.10.0","constraint":"^7"},
            {"name":"psr/log","installed":"3.0.1","latest":"3.0.2","constraint":"^3"}
        ])
    );
    assert_eq!(
        args_file(dir.path()),
        vec!["outdated", "--format=json", "--direct", "--no-interaction"]
    );
}

#[test]
fn validate_reports_valid_warnings_and_invalid() {
    let dir = TempDir::new("validate");
    write_project(dir.path());
    let composer = fake_composer(dir.path());

    let valid = run(
        dir.path(),
        &composer,
        &["validate", "--json"],
        &[("FAKE_VALIDATE_STDOUT", "./composer.json is valid\n")],
    );
    assert!(valid.status.success());
    assert_eq!(
        json(&valid),
        serde_json::json!({"valid":true,"errors":[],"warnings":[]})
    );

    let warnings = run(
        dir.path(),
        &composer,
        &["validate", "--json"],
        &[
            (
                "FAKE_VALIDATE_STDOUT",
                "./composer.json is valid, but with a few warnings\n",
            ),
            (
                "FAKE_VALIDATE_STDERR",
                "# General warnings\n- exact version\n",
            ),
        ],
    );
    assert!(warnings.status.success());
    assert_eq!(
        json(&warnings),
        serde_json::json!({"valid":true,"errors":[],"warnings":["exact version"]})
    );

    let invalid = run(
        dir.path(),
        &composer,
        &["validate", "--json"],
        &[
            (
                "FAKE_VALIDATE_STDERR",
                "In Factory.php line 317:\n\n  \"./composer.json\" does not match the expected JSON schema:\n   - name : bad\n\nvalidate [--no-check-all]\n",
            ),
            ("FAKE_EXIT", "1"),
        ],
    );
    assert_eq!(invalid.status.code(), Some(1));
    let value = json(&invalid);
    assert_eq!(value["valid"], false);
    assert_eq!(value["errors"].as_array().unwrap().len(), 1);
    assert!(value["errors"][0]
        .as_str()
        .unwrap()
        .contains("does not match"));

    assert_eq!(
        args_file(dir.path()),
        vec![
            "validate",
            "--no-check-publish",
            "--no-interaction",
            "validate",
            "--no-check-publish",
            "--no-interaction",
            "validate",
            "--no-check-publish",
            "--no-interaction",
        ]
    );
}

#[test]
fn show_strips_unneeded_fields() {
    let dir = TempDir::new("show");
    write_project(dir.path());
    let composer = fake_composer(dir.path());

    let output = run(
        dir.path(),
        &composer,
        &["show", "psr/log", "--json"],
        &[(
            "FAKE_SHOW",
            r#"{"name":"psr/log","description":"Common interface","versions":["3.0.2"],"requires":{"php":">=8.0"},"keywords":["log"]}"#,
        )],
    );
    assert!(output.status.success());
    assert_eq!(
        json(&output),
        serde_json::json!({
            "name":"psr/log",
            "versions":["3.0.2"],
            "description":"Common interface",
            "requires":{"php":">=8.0"}
        })
    );
    assert_eq!(
        args_file(dir.path()),
        vec!["show", "--format=json", "--no-interaction", "--", "psr/log"]
    );
}

#[test]
fn run_captures_output_and_exit_code() {
    let dir = TempDir::new("run");
    write_project(dir.path());
    let composer = fake_composer(dir.path());

    let output = run(
        dir.path(),
        &composer,
        &["run", "test", "--json"],
        &[("FAKE_EXIT", "7")],
    );
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(
        json(&output),
        serde_json::json!({
            "script":"test",
            "exit_code":7,
            "stdout":"script-out\n",
            "stderr":"script-err\n"
        })
    );
    assert_eq!(
        args_file(dir.path()),
        vec!["run-script", "--no-interaction", "--", "test"]
    );
}

#[test]
fn require_refuses_write_without_dry_run() {
    let dir = TempDir::new("require-refuse");
    write_project(dir.path());
    let composer = fake_composer(dir.path());

    let output = run(
        dir.path(),
        &composer,
        &["require", "psr/container:^2", "--json"],
        &[],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("refusing to require"));
    assert!(args_file(dir.path()).is_empty());
}

#[test]
fn require_dry_run_passes_flags_after_double_dash() {
    let dir = TempDir::new("require-dry");
    write_project(dir.path());
    let composer = fake_composer(dir.path());

    let output = run(
        dir.path(),
        &composer,
        &[
            "require",
            "psr/container:^2",
            "--dev",
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert!(output.status.success());
    assert_eq!(
        json(&output),
        serde_json::json!({
            "action":"require",
            "package":"psr/container:^2",
            "dev":true,
            "dry_run":true,
            "exit_code":0,
            "stdout":"require-dry\n",
            "stderr":""
        })
    );
    assert_eq!(
        args_file(dir.path()),
        vec![
            "require",
            "--no-interaction",
            "--dev",
            "--dry-run",
            "--",
            "psr/container:^2"
        ]
    );
}

#[test]
fn remove_dry_run_is_allowed() {
    let dir = TempDir::new("remove-dry");
    write_project(dir.path());
    let composer = fake_composer(dir.path());

    let output = run(
        dir.path(),
        &composer,
        &["remove", "psr/log", "--dry-run", "--json"],
        &[],
    );
    assert!(output.status.success());
    assert_eq!(
        args_file(dir.path()),
        vec!["remove", "--no-interaction", "--dry-run", "--", "psr/log"]
    );
}

#[test]
fn missing_composer_reports_install_hint() {
    let dir = TempDir::new("missing");
    write_project(dir.path());
    let composer = fake_composer(dir.path());

    let output = run(
        dir.path(),
        &composer,
        &["validate", "--json"],
        &[(
            "PHP_COMPOSER_BIN",
            "/nonexistent/php-composer-test-composer",
        )],
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("composer executable not found"));
    assert!(stderr.contains("https://getcomposer.org/download/"));
}

#[test]
fn walks_up_to_find_composer_json() {
    let dir = TempDir::new("walk");
    write_project(dir.path());
    let nested = dir.path().join("a/b");
    fs::create_dir_all(&nested).unwrap();
    let composer = fake_composer(dir.path());

    let output = run(&nested, &composer, &["list", "--json"], &[]);
    assert!(output.status.success());
    assert_eq!(json(&output).as_array().unwrap().len(), 3);
}

#[test]
fn rejects_option_like_operands() {
    let dir = TempDir::new("safety");
    write_project(dir.path());
    let composer = fake_composer(dir.path());

    let require = run(dir.path(), &composer, &["require", "--", "--dev"], &[]);
    assert_eq!(require.status.code(), Some(2));

    let show = run(dir.path(), &composer, &["show", "--", "-x/y"], &[]);
    assert_eq!(show.status.code(), Some(2));

    let unknown = run(dir.path(), &composer, &["require", "--nope"], &[]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(args_file(dir.path()).is_empty());
}
