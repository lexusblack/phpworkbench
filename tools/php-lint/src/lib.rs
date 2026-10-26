use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

pub const USAGE: &str = "\
php-lint - PHPStan bridge

Usage:
  php-lint check [--dir PATH] [--file PATH] [--level N] [--json]
                 [--memory 1G] [--phpstan PATH]

Options:
  --dir PATH      Project directory to analyse (default: .)
  --file PATH     Analyse a single file (relative to --dir)
  --level N       PHPStan rule level passed to the analyser
  --json          Emit normalized JSON instead of human-readable lines
  --memory SIZE   PHPStan memory limit (default: 1G)
  --phpstan PATH  Explicit path to the PHPStan executable
  -h, --help      Show this help

Exit codes:
  0  no errors
  1  PHPStan reported errors
  2  tool or configuration error
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckOptions {
    pub dir: PathBuf,
    pub file: Option<PathBuf>,
    pub level: Option<u32>,
    pub json: bool,
    pub memory: String,
    pub phpstan: Option<PathBuf>,
}

impl Default for CheckOptions {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("."),
            file: None,
            level: None,
            json: false,
            memory: "1G".to_string(),
            phpstan: None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
    Check(CheckOptions),
    Help,
}

pub fn parse_args<I, S>(args: I) -> Result<Invocation>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut args = args.into_iter().map(Into::into);
    let Some(command) = args.next() else {
        bail!("missing command\n\n{USAGE}");
    };

    match command.as_str() {
        "-h" | "--help" | "help" => return Ok(Invocation::Help),
        "check" => {}
        other => bail!("unknown command '{other}'\n\n{USAGE}"),
    }

    let mut options = CheckOptions::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => options.dir = PathBuf::from(next_value(&mut args, "--dir")?),
            "--file" => options.file = Some(PathBuf::from(next_value(&mut args, "--file")?)),
            "--level" => {
                let raw = next_value(&mut args, "--level")?;
                options.level = Some(
                    raw.parse()
                        .with_context(|| format!("invalid --level value '{raw}'"))?,
                );
            }
            "--memory" => options.memory = next_value(&mut args, "--memory")?,
            "--phpstan" => {
                options.phpstan = Some(PathBuf::from(next_value(&mut args, "--phpstan")?))
            }
            "--json" => options.json = true,
            "-h" | "--help" => return Ok(Invocation::Help),
            other => bail!("unknown option '{other}'\n\n{USAGE}"),
        }
    }

    Ok(Invocation::Check(options))
}

fn next_value<I>(args: &mut I, flag: &str) -> Result<String>
where
    I: Iterator<Item = String>,
{
    match args.next() {
        Some(value) if !value.starts_with("--") => Ok(value),
        _ => bail!("missing value for {flag}"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LintError {
    pub file: String,
    pub line: u32,
    pub message: String,
    pub identifier: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Totals {
    pub files: usize,
    pub errors: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    pub errors: Vec<LintError>,
    pub totals: Totals,
    pub exit_code: i32,
}

#[derive(Deserialize)]
struct RawReport {
    #[serde(default)]
    files: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    errors: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawFile {
    #[serde(default)]
    messages: Vec<RawMessage>,
}

#[derive(Deserialize)]
struct RawMessage {
    message: String,
    #[serde(default)]
    line: u32,
    #[serde(default)]
    identifier: Option<String>,
}

pub fn normalize(raw: &str, exit_code: i32) -> Result<Report> {
    let json = extract_json(raw)
        .context("PHPStan did not produce JSON output; run it manually to inspect the error")?;
    let parsed: RawReport = serde_json::from_str(json).context("invalid PHPStan JSON output")?;

    if !parsed.errors.is_empty() {
        let details = parsed
            .errors
            .iter()
            .map(
                |value| match value.get("message").and_then(|m| m.as_str()) {
                    Some(message) => message.to_string(),
                    None => value.to_string(),
                },
            )
            .collect::<Vec<_>>()
            .join("; ");
        bail!("PHPStan reported an internal error: {details}");
    }

    let mut errors = Vec::new();
    let mut files_with_errors = 0;
    for (file, value) in &parsed.files {
        let raw_file: RawFile = serde_json::from_value(value.clone())
            .with_context(|| format!("invalid PHPStan result for {file}"))?;
        if raw_file.messages.is_empty() {
            continue;
        }
        files_with_errors += 1;
        for message in raw_file.messages {
            errors.push(LintError {
                file: file.clone(),
                line: message.line,
                message: message.message,
                identifier: message.identifier,
            });
        }
    }
    errors.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));

    Ok(Report {
        totals: Totals {
            files: files_with_errors,
            errors: errors.len(),
        },
        exit_code,
        errors,
    })
}

fn extract_json(raw: &str) -> Option<&str> {
    let mut starts = Vec::new();
    let mut offset = 0;
    for line in raw.split_inclusive('\n') {
        if line.trim_start().starts_with('{') {
            starts.push(offset + (line.len() - line.trim_start().len()));
        }
        offset += line.len();
    }
    for start in starts.into_iter().rev() {
        let candidate = raw[start..].trim();
        if serde_json::from_str::<serde_json::Value>(candidate).is_ok() {
            return Some(candidate);
        }
    }
    None
}

pub fn render_human(report: &Report) -> String {
    let mut output = String::new();
    for error in &report.errors {
        match &error.identifier {
            Some(identifier) => {
                let _ = writeln!(
                    output,
                    "{}:{}: {} [{}]",
                    error.file, error.line, error.message, identifier
                );
            }
            None => {
                let _ = writeln!(output, "{}:{}: {}", error.file, error.line, error.message);
            }
        }
    }
    output
}

pub fn human_exit_code(report: &Report) -> i32 {
    if !report.errors.is_empty() {
        1
    } else if report.exit_code == 0 {
        0
    } else {
        2
    }
}

pub fn find_phpstan(start: &Path) -> Option<PathBuf> {
    let mut dir = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    loop {
        let candidate = dir.join("vendor").join("bin").join("phpstan");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

pub fn missing_phpstan_message(searched_from: &Path) -> String {
    format!(
        "PHPStan not found: no vendor/bin/phpstan in {} or any parent directory.\n\
         Install it with: composer require --dev phpstan/phpstan",
        searched_from.display()
    )
}

pub fn missing_phpstan_path_message(path: &Path) -> String {
    format!(
        "PHPStan not found at {}.\n\
         Install it with: composer require --dev phpstan/phpstan",
        path.display()
    )
}

pub fn validate_dir(dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        bail!("target directory does not exist: {}", dir.display());
    }
    Ok(())
}

pub fn analyse_path(options: &CheckOptions) -> PathBuf {
    options.file.clone().unwrap_or_else(|| PathBuf::from("."))
}

pub fn resolve_phpstan(options: &CheckOptions) -> Result<PathBuf> {
    if let Some(explicit) = &options.phpstan {
        if explicit.is_file() {
            return Ok(explicit.canonicalize().unwrap_or_else(|_| explicit.clone()));
        }
        if let Some(found) = search_path(explicit) {
            return Ok(found);
        }
        bail!(missing_phpstan_path_message(explicit));
    }

    find_phpstan(&options.dir).with_context(|| missing_phpstan_message(&options.dir))
}

fn search_path(explicit: &Path) -> Option<PathBuf> {
    if explicit.components().count() == 1 {
        if let Some(paths) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&paths) {
                let candidate = dir.join(explicit);
                if candidate.is_file() {
                    return Some(candidate.canonicalize().unwrap_or(candidate));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"totals":{"errors":0,"file_errors":2},"files":{"/p/src/A.php":{"errors":2,"messages":[{"message":"Undefined variable: $x","line":11,"ignorable":true,"identifier":"variable.undefined"},{"message":"Something odd","line":3}]}},"errors":[]}"#;

    #[test]
    fn normalizes_messages_and_totals() {
        let report = normalize(SAMPLE, 1).unwrap();
        assert_eq!(report.totals.files, 1);
        assert_eq!(report.totals.errors, 2);
        assert_eq!(report.exit_code, 1);
        assert_eq!(report.errors[0].line, 3);
        assert_eq!(report.errors[0].identifier, None);
        assert_eq!(report.errors[1].line, 11);
        assert_eq!(
            report.errors[1].identifier.as_deref(),
            Some("variable.undefined")
        );
    }

    #[test]
    fn ignores_phpstan_preamble() {
        let raw = format!(
            "Instructions for interpreting errors\n--------- https://phpstan.org\n{SAMPLE}\n"
        );
        let report = normalize(&raw, 1).unwrap();
        assert_eq!(report.totals.errors, 2);
    }

    #[test]
    fn clean_report_is_empty() {
        let report = normalize(
            r#"{"totals":{"errors":0,"file_errors":0},"files":{},"errors":[]}"#,
            0,
        )
        .unwrap();
        assert!(report.errors.is_empty());
        assert_eq!(
            report.totals,
            Totals {
                files: 0,
                errors: 0
            }
        );
        assert_eq!(human_exit_code(&report), 0);
    }

    #[test]
    fn human_lines_include_optional_identifier() {
        let report = normalize(SAMPLE, 1).unwrap();
        let rendered = render_human(&report);
        assert!(rendered.contains("/p/src/A.php:3: Something odd\n"));
        assert!(rendered.contains("/p/src/A.php:11: Undefined variable: $x [variable.undefined]\n"));
    }

    #[test]
    fn parses_default_check_options() {
        let invocation = parse_args(["check"]).unwrap();
        assert_eq!(invocation, Invocation::Check(CheckOptions::default()));
    }

    #[test]
    fn parses_all_check_options() {
        let invocation = parse_args([
            "check",
            "--dir",
            "fixtures/lint",
            "--file",
            "src/Clean.php",
            "--level",
            "6",
            "--memory",
            "512M",
            "--phpstan",
            "/opt/phpstan",
            "--json",
        ])
        .unwrap();
        let Invocation::Check(options) = invocation else {
            panic!("expected check");
        };
        assert_eq!(options.dir, PathBuf::from("fixtures/lint"));
        assert_eq!(options.file, Some(PathBuf::from("src/Clean.php")));
        assert_eq!(options.level, Some(6));
        assert_eq!(options.memory, "512M");
        assert_eq!(options.phpstan, Some(PathBuf::from("/opt/phpstan")));
        assert!(options.json);
    }

    #[test]
    fn rejects_unknown_command_and_option() {
        assert!(parse_args(["lint"]).is_err());
        assert!(parse_args(["check", "--bogus"]).is_err());
        assert!(parse_args(["check", "--level", "high"]).is_err());
        assert!(parse_args(["check", "--dir"]).is_err());
    }

    #[test]
    fn internal_errors_are_fatal() {
        let raw = r#"{"totals":{"errors":1,"file_errors":0},"files":{},"errors":[{"message":"Config not found"}]}"#;
        let error = normalize(raw, 1).unwrap_err().to_string();
        assert!(error.contains("Config not found"));
    }

    #[test]
    fn walks_up_to_find_phpstan() {
        let base = std::env::temp_dir().join(format!("php-lint-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let nested = base.join("a").join("b");
        std::fs::create_dir_all(nested.join("vendor").join("bin")).unwrap();
        std::fs::write(nested.join("vendor/bin/phpstan"), "#!/bin/sh\n").unwrap();
        let found = find_phpstan(&nested.join("..").join("b"));
        assert_eq!(
            found,
            Some(nested.canonicalize().unwrap().join("vendor/bin/phpstan"))
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
