use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{bail, Context, Result};
use php_test::{
    find_tool, has_failures, list_test_files, parse_args, parse_clover, parse_junit, render_run,
    tool_args, validate_dir, Coverage, Invocation, ListOptions, RunOptions, Tool,
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
    if options.coverage && options.no_coverage {
        bail!("--coverage and --no-coverage cannot be combined");
    }
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

    let junit = TempFile::new("junit");
    let coverage_file = options.coverage.then(|| TempFile::new("coverage"));
    let clover = coverage_file.as_ref().map(TempFile::path);

    let mut output = run_tool(tool, &binary, options, junit.path(), clover)?;
    let mut collect_coverage = options.coverage;

    if collect_coverage && !has_junit(junit.path()) {
        eprintln!(
            "php-test: warning: coverage failed and produced no test results; \
             retrying without coverage (install Xdebug or PCOV for coverage)"
        );
        let mut fallback = options.clone();
        fallback.coverage = false;
        output = run_tool(tool, &binary, &fallback, junit.path(), None)?;
        collect_coverage = false;
    }

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
    let mut report = parse_junit(&xml, &options.dir, tool)?;

    if collect_coverage {
        if let Some(coverage_file) = &coverage_file {
            match read_coverage(coverage_file.path(), &options.dir) {
                Ok(coverage) => report.coverage = Some(coverage),
                Err(error) => {
                    eprintln!("php-test: warning: coverage unavailable: {error:#}");
                }
            }
        }
    }

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

fn run_tool(
    tool: Tool,
    binary: &std::path::Path,
    options: &RunOptions,
    junit: &std::path::Path,
    clover: Option<&std::path::Path>,
) -> Result<std::process::Output> {
    let mut command = Command::new(binary);
    command
        .current_dir(&options.dir)
        .args(tool_args(options, junit, clover));
    if clover.is_some() {
        command.env("XDEBUG_MODE", "coverage");
    }
    command.output().with_context(|| {
        format!(
            "failed to run {} at {}.\n\
             Ensure PHP is available on PATH.",
            tool.as_str(),
            binary.display()
        )
    })
}

fn has_junit(path: &std::path::Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.len() > 0)
        .unwrap_or(false)
}

fn read_coverage(path: &std::path::Path, dir: &std::path::Path) -> Result<Coverage> {
    if !path.is_file() {
        bail!(
            "no Clover report was produced; \
             install a coverage driver such as Xdebug or PCOV"
        );
    }
    let xml =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    parse_clover(&xml, dir).context("failed to parse Clover coverage report")
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

struct TempFile {
    path: PathBuf,
}

impl TempFile {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("php-test-{label}-{}-{id}.xml", std::process::id()));
        let _ = fs::remove_file(&path);
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
