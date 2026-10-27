use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{bail, Context, Result};
use php_test::{
    find_tool, has_failures, list_test_files, parse_args, parse_junit, render_run, tool_args,
    validate_dir, Invocation, ListOptions, RunOptions, Tool,
};
use serde_json::json;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(&args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("php-test: {error:#}");
            2
        }
    };
    std::process::exit(code);
}

fn run(args: &[String]) -> Result<i32> {
    match parse_args(args.iter().cloned())? {
        Invocation::Help => {
            print!("{}", php_test::USAGE);
            Ok(0)
        }
        Invocation::List(options) => list(&options),
        Invocation::Run(options) => execute(&options),
    }
}

fn list(options: &ListOptions) -> Result<i32> {
    validate_dir(&options.dir)?;
    let tool = find_tool(&options.dir)?
        .map(|(tool, _)| tool)
        .unwrap_or(Tool::Phpunit);
    let files = list_test_files(&options.dir)?;

    if options.json {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "tool": tool.as_str(),
                "files": files,
            }))?
        );
    } else {
        for file in &files {
            println!("{file}");
        }
    }
    Ok(0)
}

fn execute(options: &RunOptions) -> Result<i32> {
    validate_dir(&options.dir)?;
    if let Some(file) = &options.file {
        let path = options.dir.join(file);
        if !path.is_file() {
            bail!("test file does not exist: {}", path.display());
        }
    }

    let Some((tool, binary)) = find_tool(&options.dir)? else {
        bail!(
            "could not find vendor/bin/phpunit or vendor/bin/pest under {}.\n\
             Install it with: composer install",
            options.dir.display()
        );
    };

    let junit = TempJunit::new();
    let output = Command::new(&binary)
        .current_dir(&options.dir)
        .args(tool_args(options, junit.path()))
        .output()
        .with_context(|| {
            format!(
                "failed to run {} at {}.\n\
                 Ensure PHP is available on PATH.",
                tool.as_str(),
                binary.display()
            )
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let exit_code = output.status.code().unwrap_or(2);

    if !junit.path().is_file() {
        let details = combine_output(&stdout, &stderr);
        if details.is_empty() {
            bail!(
                "{} exited with code {exit_code} without producing JUnit output",
                tool.as_str()
            );
        }
        bail!(
            "{} exited with code {exit_code} without producing JUnit output:\n{details}",
            tool.as_str()
        );
    }

    let xml = fs::read_to_string(junit.path())
        .with_context(|| format!("failed to read {}", junit.path().display()))?;
    let report = parse_junit(&xml, &options.dir, tool)?;

    if options.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        print!("{}", render_run(&report));
    }

    if has_failures(&report) {
        return Ok(1);
    }
    if exit_code != 0 {
        let details = combine_output(&stdout, &stderr);
        if details.is_empty() {
            bail!(
                "{} exited with code {exit_code} although no test failed",
                tool.as_str()
            );
        }
        bail!(
            "{} exited with code {exit_code} although no test failed:\n{details}",
            tool.as_str()
        );
    }
    Ok(0)
}

fn combine_output(stdout: &str, stderr: &str) -> String {
    let stdout = stdout.trim();
    let stderr = stderr.trim();
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (false, false) => format!("{stdout}\n{stderr}"),
    }
}

struct TempJunit {
    path: PathBuf,
}

impl TempJunit {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("php-test-{}-{id}.xml", std::process::id()));
        let _ = fs::remove_file(&path);
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempJunit {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
