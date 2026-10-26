#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("php-lint-cli-{name}-{}-{id}", std::process::id()));
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

fn fake_phpstan(dir: &Path) {
    let bin = dir.join("vendor/bin");
    fs::create_dir_all(&bin).unwrap();
    let script = bin.join("phpstan");
    fs::write(
        &script,
        "#!/bin/sh\n\
         if [ -n \"$FAKE_ARGS_FILE\" ]; then printf '%s\\n' \"$@\" > \"$FAKE_ARGS_FILE\"; fi\n\
         cat \"$FAKE_JSON_FILE\"\n\
         exit \"$FAKE_EXIT_CODE\"\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).unwrap();
}

fn run(dir: &Path, args: &[&str], json: &str, exit: i32) -> (Output, Vec<String>) {
    let args_file = dir.join("args.txt");
    let json_file = dir.join("result.json");
    fs::write(&json_file, json).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_php-lint"))
        .current_dir(dir)
        .args(args)
        .env("FAKE_ARGS_FILE", &args_file)
        .env("FAKE_JSON_FILE", &json_file)
        .env("FAKE_EXIT_CODE", exit.to_string())
        .output()
        .unwrap();
    let recorded = fs::read_to_string(&args_file)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    (output, recorded)
}

const ONE_ERROR: &str = r#"{"totals":{"errors":0,"file_errors":1},"files":{"/p/src/Broken.php":{"errors":1,"messages":[{"message":"Undefined variable: $x","line":7,"ignorable":true,"identifier":"variable.undefined"}]}},"errors":[]}"#;
const CLEAN: &str = r#"{"totals":{"errors":0,"file_errors":0},"files":{},"errors":[]}"#;

#[test]
fn discovers_phpstan_and_emits_normalized_json() {
    let temp = TempDir::new("discover");
    fake_phpstan(temp.path());

    let (output, recorded) = run(
        temp.path(),
        &["check", "--json", "--level", "6"],
        ONE_ERROR,
        1,
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["totals"]["files"], 1);
    assert_eq!(report["totals"]["errors"], 1);
    assert_eq!(report["exit_code"], 1);
    assert_eq!(report["errors"][0]["file"], "/p/src/Broken.php");
    assert_eq!(report["errors"][0]["line"], 7);
    assert_eq!(report["errors"][0]["identifier"], "variable.undefined");

    assert!(recorded.contains(&"analyse".to_string()));
    assert!(recorded.contains(&"--error-format=json".to_string()));
    assert!(recorded.contains(&"--no-progress".to_string()));
    assert!(recorded.contains(&"--no-interaction".to_string()));
    assert!(recorded.contains(&"--memory-limit=1G".to_string()));
    assert!(recorded.contains(&"--level".to_string()));
    assert!(recorded.contains(&"6".to_string()));
}

#[test]
fn limits_analysis_to_one_file_and_exits_zero_when_clean() {
    let temp = TempDir::new("file");
    fake_phpstan(temp.path());
    fs::create_dir_all(temp.path().join("src")).unwrap();
    fs::write(temp.path().join("src/Clean.php"), "<?php\n").unwrap();

    let (output, recorded) = run(temp.path(), &["check", "--file", "src/Clean.php"], CLEAN, 0);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert!(recorded.contains(&"src/Clean.php".to_string()));
    assert!(!recorded.contains(&".".to_string()));
}

#[test]
fn human_output_matches_expected_format() {
    let temp = TempDir::new("human");
    fake_phpstan(temp.path());

    let (output, _) = run(temp.path(), &["check"], ONE_ERROR, 1);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "/p/src/Broken.php:7: Undefined variable: $x [variable.undefined]\n"
    );
}

#[test]
fn relative_explicit_phpstan_is_resolved_before_changing_directory() {
    let temp = TempDir::new("relative");
    fake_phpstan(temp.path());

    let (output, recorded) = run(
        temp.path(),
        &["check", "--phpstan", "vendor/bin/phpstan", "--json"],
        ONE_ERROR,
        1,
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(recorded.contains(&"analyse".to_string()));
}

#[test]
fn missing_phpstan_exits_two_with_install_hint() {
    let temp = TempDir::new("missing");
    let missing = temp.path().join("no-such-phpstan");

    let output = Command::new(env!("CARGO_BIN_EXE_php-lint"))
        .current_dir(temp.path())
        .args([
            "check",
            "--phpstan",
            missing.to_str().unwrap(),
            "--level",
            "6",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("PHPStan not found at"), "{stderr}");
    assert!(
        stderr.contains("composer require --dev phpstan/phpstan"),
        "{stderr}"
    );
}

#[test]
fn missing_file_is_a_tool_error() {
    let temp = TempDir::new("nofile");
    fake_phpstan(temp.path());

    let (output, _) = run(temp.path(), &["check", "--file", "src/Nope.php"], CLEAN, 0);

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("target file does not exist"));
}
