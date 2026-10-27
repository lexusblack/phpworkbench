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
        let path =
            std::env::temp_dir().join(format!("php-test-cli-{name}-{}-{id}", std::process::id()));
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

fn fake_tool(dir: &Path, name: &str) {
    let bin = dir.join("vendor/bin");
    fs::create_dir_all(&bin).unwrap();
    let script = bin.join(name);
    fs::write(
        &script,
        "#!/bin/sh\n\
         junit=\"\"\n\
         previous=\"\"\n\
         for arg in \"$@\"; do\n\
           if [ \"$previous\" = \"--log-junit\" ]; then junit=\"$arg\"; fi\n\
           if [ -n \"$FAKE_ARGS_FILE\" ]; then printf '%s\\n' \"$arg\" >> \"$FAKE_ARGS_FILE\"; fi\n\
           previous=\"$arg\"\n\
         done\n\
         if [ -n \"$junit\" ] && [ -n \"$FAKE_JUNIT\" ]; then cp \"$FAKE_JUNIT\" \"$junit\"; fi\n\
         exit \"${FAKE_EXIT:-0}\"\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).unwrap();
}

fn run(dir: &Path, args: &[&str], junit: Option<&str>, exit: i32) -> (Output, Vec<String>) {
    let args_file = dir.join("args.txt");
    let _ = fs::remove_file(&args_file);
    let mut command = Command::new(env!("CARGO_BIN_EXE_php-test"));
    command
        .arg(args[0])
        .arg("--dir")
        .arg(dir)
        .args(&args[1..])
        .env("FAKE_EXIT", exit.to_string())
        .env("FAKE_ARGS_FILE", &args_file);
    if let Some(junit) = junit {
        command.env("FAKE_JUNIT", junit);
    }
    let output = command.output().unwrap();
    let captured = fs::read_to_string(&args_file)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    (output, captured)
}

const PASSING_JUNIT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="App\Tests\CalculatorTest" file="/tmp/does-not-exist/tests/CalculatorTest.php" tests="2">
    <testcase name="testAddsNumbers" class="App\Tests\CalculatorTest" time="0.001"/>
    <testcase name="testSubtractsNumbers" class="App\Tests\CalculatorTest" time="0.002"/>
  </testsuite>
</testsuites>"#;

const FAILING_JUNIT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="outer">
    <testsuite name="App\Tests\FailingTest">
      <testcase name="testFailsOnPurpose" class="App\Tests\FailingTest" time="0.001">
        <failure type="ExpectationFailed">App\Tests\FailingTest::testFailsOnPurpose
Failed asserting that 5 is identical to 4.

/tmp/FailingTest.php:12</failure>
      </testcase>
      <testcase name="testIsSkipped" class="App\Tests\FailingTest" time="0.000">
        <skipped>skipped for fixture</skipped>
      </testcase>
    </testsuite>
  </testsuite>
</testsuites>"#;

fn write_junit(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, contents).unwrap();
    path
}

#[test]
fn run_json_success_and_arguments() {
    let dir = TempDir::new("run-pass");
    fake_tool(dir.path(), "phpunit");
    fs::create_dir_all(dir.path().join("tests")).unwrap();
    fs::write(dir.path().join("tests/CalculatorTest.php"), "<?php").unwrap();
    let junit = write_junit(dir.path(), "pass.xml", PASSING_JUNIT);

    let (output, args) = run(
        dir.path(),
        &[
            "run",
            "--filter",
            "Calculator",
            "--file",
            "tests/CalculatorTest.php",
            "--group",
            "unit",
            "--json",
            "--no-coverage",
        ],
        junit.to_str(),
        0,
    );

    assert_eq!(output.status.code(), Some(0));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["tool"], "phpunit");
    assert_eq!(value["tests"].as_array().unwrap().len(), 2);
    assert_eq!(value["tests"][0]["status"], "pass");
    assert_eq!(value["tests"][0]["class"], "App\\Tests\\CalculatorTest");
    assert_eq!(value["summary"]["passed"], 2);
    assert_eq!(value["summary"]["total"], 2);

    assert_eq!(args[0], "--log-junit");
    assert!(args[1].ends_with(".xml"));
    assert!(args.contains(&"--filter".to_string()));
    assert!(args.contains(&"Calculator".to_string()));
    assert!(args.contains(&"--group".to_string()));
    assert!(args.contains(&"--no-coverage".to_string()));
    assert_eq!(args.last().unwrap(), "tests/CalculatorTest.php");
}

#[test]
fn run_human_reports_failures_and_skips() {
    let dir = TempDir::new("run-fail");
    fake_tool(dir.path(), "phpunit");
    let junit = write_junit(dir.path(), "fail.xml", FAILING_JUNIT);

    let (output, _) = run(dir.path(), &["run"], junit.to_str(), 1);
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("FAIL App\\Tests\\FailingTest::testFailsOnPurpose"));
    assert!(stdout.contains("Failed asserting that 5 is identical to 4."));
    assert!(stdout.contains("SKIP App\\Tests\\FailingTest::testIsSkipped"));
    assert!(stdout.contains("Tests: 2 | Passed: 0 | Failed: 1 | Errors: 0 | Skipped: 1"));
}

#[test]
fn run_without_junit_is_a_tool_error() {
    let dir = TempDir::new("run-no-junit");
    fake_tool(dir.path(), "phpunit");

    let (output, _) = run(dir.path(), &["run"], None, 3);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("without producing JUnit output"));
}

#[test]
fn list_discovers_files_and_reports_pest() {
    let dir = TempDir::new("list");
    fake_tool(dir.path(), "phpunit");
    fake_tool(dir.path(), "pest");
    fs::create_dir_all(dir.path().join("tests")).unwrap();
    fs::write(dir.path().join("tests/CalculatorTest.php"), "<?php").unwrap();
    fs::write(dir.path().join("tests/FailingTest.php"), "<?php").unwrap();
    fs::write(dir.path().join("tests/Helper.php"), "<?php").unwrap();

    let (output, _) = run(dir.path(), &["list", "--json"], None, 0);
    assert_eq!(output.status.code(), Some(0));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["tool"], "pest");
    assert_eq!(
        value["files"],
        serde_json::json!(["tests/CalculatorTest.php", "tests/FailingTest.php"])
    );
}
