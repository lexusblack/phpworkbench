use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Serialize;

pub const USAGE: &str = "\
php-test - PHPUnit/Pest test runner bridge

Usage:
  php-test list [--dir PATH] [--json]
  php-test run  [--dir PATH] [--filter PATTERN] [--file REL]
                [--group NAME] [--coverage] [--json] [--no-coverage]

Commands:
  list   List test files discovered under tests/ and phpunit.xml testsuites
  run    Run PHPUnit or Pest with a JUnit log and report normalized results

Options:
  --dir PATH       Project directory (default: .)
  --filter PATTERN Only run tests whose name matches PATTERN
  --file REL       Run a single test file, relative to --dir
  --group NAME     Only run tests in the given group
  --coverage       Collect line coverage with a Clover report
  --json           Emit normalized JSON
  --no-coverage    Disable coverage collection
  -h, --help       Show this help

The runner walks up from --dir to find vendor/bin/pest or vendor/bin/phpunit.
Pest is preferred when both are installed. PHP must be available on PATH.

Exit codes:
  0  all tests passed
  1  at least one test failed or errored
  2  tool or configuration error
";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    Phpunit,
    Pest,
}

impl Tool {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Phpunit => "phpunit",
            Self::Pest => "pest",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListOptions {
    pub dir: PathBuf,
    pub json: bool,
}

impl Default for ListOptions {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("."),
            json: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOptions {
    pub dir: PathBuf,
    pub filter: Option<String>,
    pub file: Option<String>,
    pub group: Option<String>,
    pub json: bool,
    pub coverage: bool,
    pub no_coverage: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("."),
            filter: None,
            file: None,
            group: None,
            json: false,
            coverage: false,
            no_coverage: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    List(ListOptions),
    Run(RunOptions),
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

    let is_run = match command.as_str() {
        "-h" | "--help" | "help" => return Ok(Invocation::Help),
        "list" => false,
        "run" => true,
        other => bail!("unknown command '{other}'\n\n{USAGE}"),
    };

    let mut dir = PathBuf::from(".");
    let mut json = false;
    let mut filter = None;
    let mut file = None;
    let mut group = None;
    let mut coverage = false;
    let mut no_coverage = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Invocation::Help),
            "--dir" => dir = PathBuf::from(next_value(&mut args, "--dir")?),
            "--json" => json = true,
            "--filter" if is_run => filter = Some(next_value(&mut args, "--filter")?),
            "--file" if is_run => file = Some(next_value(&mut args, "--file")?),
            "--group" if is_run => group = Some(next_value(&mut args, "--group")?),
            "--coverage" if is_run => coverage = true,
            "--no-coverage" if is_run => no_coverage = true,
            other => bail!("unknown option '{other}' for {command}\n\n{USAGE}"),
        }
    }

    if is_run {
        Ok(Invocation::Run(RunOptions {
            dir,
            filter,
            file,
            group,
            json,
            coverage,
            no_coverage,
        }))
    } else {
        Ok(Invocation::List(ListOptions { dir, json }))
    }
}

fn next_value<I>(args: &mut I, option: &str) -> Result<String>
where
    I: Iterator<Item = String>,
{
    args.next()
        .with_context(|| format!("missing value for {option}"))
}

pub fn validate_dir(dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        bail!("project directory does not exist: {}", dir.display());
    }
    Ok(())
}

/// Walk up from `start` looking for `vendor/bin/pest` or `vendor/bin/phpunit`.
pub fn find_tool(start: &Path) -> Result<Option<(Tool, PathBuf)>> {
    validate_dir(start)?;
    let mut dir = fs::canonicalize(start)
        .with_context(|| format!("failed to resolve {}", start.display()))?;

    loop {
        let bin = dir.join("vendor").join("bin");
        let pest = bin.join("pest");
        if pest.is_file() {
            return Ok(Some((Tool::Pest, pest)));
        }
        let phpunit = bin.join("phpunit");
        if phpunit.is_file() {
            return Ok(Some((Tool::Phpunit, phpunit)));
        }
        if !dir.pop() {
            break;
        }
    }
    Ok(None)
}

pub fn list_test_files(dir: &Path) -> Result<Vec<String>> {
    validate_dir(dir)?;
    let base =
        fs::canonicalize(dir).with_context(|| format!("failed to resolve {}", dir.display()))?;
    let mut files = BTreeSet::new();

    let tests = base.join("tests");
    if tests.is_dir() {
        walk_test_files(&tests, &base, &mut files)?;
    }

    if let Some((config_dir, config)) = find_phpunit_config(&base) {
        if let Ok(root) = parse_xml(&config) {
            let mut directories = Vec::new();
            let mut explicit = Vec::new();
            collect_suite_entries(&root, &mut directories, &mut explicit);

            for entry in directories {
                let path = resolve_from(&config_dir, &entry);
                if path.is_dir() {
                    walk_test_files(&path, &base, &mut files)?;
                }
            }
            for entry in explicit {
                let path = resolve_from(&config_dir, &entry);
                if path.is_file()
                    && path
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .map(|ext| ext.eq_ignore_ascii_case("php"))
                        .unwrap_or(false)
                {
                    files.insert(display_relative(&base, &path));
                }
            }
        }
    }

    Ok(files.into_iter().collect())
}

fn walk_test_files(root: &Path, base: &Path, out: &mut BTreeSet<String>) -> Result<()> {
    let mut entries = fs::read_dir(root)
        .with_context(|| format!("failed to read {}", root.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            let name = entry.file_name();
            if matches!(
                name.to_str(),
                Some("vendor") | Some("node_modules") | Some(".git")
            ) {
                continue;
            }
            walk_test_files(&path, base, out)?;
        } else if file_type.is_file() {
            let is_test = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.ends_with("Test.php"))
                .unwrap_or(false);
            if is_test {
                out.insert(display_relative(base, &path));
            }
        }
    }
    Ok(())
}

fn find_phpunit_config(start: &Path) -> Option<(PathBuf, String)> {
    let mut dir = start.to_path_buf();
    loop {
        for name in ["phpunit.xml", "phpunit.xml.dist"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                if let Ok(text) = fs::read_to_string(&candidate) {
                    return Some((dir, text));
                }
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn collect_suite_entries(root: &XmlNode, directories: &mut Vec<String>, files: &mut Vec<String>) {
    let suites = if root.name == "testsuites" {
        Some(root)
    } else {
        find_named(root, "testsuites")
    };
    if let Some(suites) = suites {
        collect_entries(suites, directories, files);
    }
}

fn collect_entries(node: &XmlNode, directories: &mut Vec<String>, files: &mut Vec<String>) {
    for child in &node.children {
        match child.name.as_str() {
            "directory" => {
                let text = child.text.trim();
                if !text.is_empty() {
                    directories.push(text.to_string());
                }
            }
            "file" => {
                let text = child.text.trim();
                if !text.is_empty() {
                    files.push(text.to_string());
                }
            }
            "testsuite" => collect_entries(child, directories, files),
            _ => {}
        }
    }
}

fn find_named<'a>(node: &'a XmlNode, name: &str) -> Option<&'a XmlNode> {
    if node.name == name {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_named(child, name))
}

fn resolve_from(base: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn display_relative(base: &Path, path: &Path) -> String {
    let absolute = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    match absolute.strip_prefix(base) {
        Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
        Err(_) => absolute.to_string_lossy().replace('\\', "/"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Fail,
    Error,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TestResult {
    pub name: String,
    pub class: String,
    pub file: String,
    pub time: f64,
    pub status: Status,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub errors: usize,
    pub skipped: usize,
    pub time: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FileCoverage {
    pub file: String,
    pub total: usize,
    pub covered: usize,
    pub percent: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Coverage {
    pub lines_total: usize,
    pub lines_covered: usize,
    pub percent: f64,
    pub files: Vec<FileCoverage>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub tool: String,
    pub tests: Vec<TestResult>,
    pub summary: Summary,
    pub coverage: Option<Coverage>,
}

pub fn parse_junit(xml: &str, project_dir: &Path, tool: Tool) -> Result<Report> {
    let root = parse_xml(xml).context("failed to parse JUnit XML")?;
    let base = fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    let mut tests = Vec::new();
    collect_cases(&root, "", "", &base, &mut tests);

    let mut summary = Summary {
        total: tests.len(),
        passed: 0,
        failed: 0,
        errors: 0,
        skipped: 0,
        time: tests.iter().map(|test| test.time).sum(),
    };
    for test in &tests {
        match test.status {
            Status::Pass => summary.passed += 1,
            Status::Fail => summary.failed += 1,
            Status::Error => summary.errors += 1,
            Status::Skip => summary.skipped += 1,
        }
    }

    Ok(Report {
        tool: tool.as_str().to_string(),
        tests,
        summary,
        coverage: None,
    })
}

/// Parse a PHPUnit/Pest Clover report into per-file line coverage.
pub fn parse_clover(xml: &str, project_dir: &Path) -> Result<Coverage> {
    let root = parse_xml(xml).context("failed to parse Clover XML")?;
    let project = find_named(&root, "project").context("Clover XML has no <project> element")?;
    let base = fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    let mut files = Vec::new();
    collect_clover_files(project, &base, &mut files);
    files.sort_by(|left, right| left.file.cmp(&right.file));

    if files.is_empty() {
        if let Some(metrics) = find_named(project, "metrics") {
            let total = metric_value(metrics, "statements");
            let covered = metric_value(metrics, "coveredstatements");
            return Ok(Coverage {
                lines_total: total,
                lines_covered: covered,
                percent: percent(covered, total),
                files,
            });
        }
    }

    let lines_total = files.iter().map(|file| file.total).sum();
    let lines_covered = files.iter().map(|file| file.covered).sum();
    Ok(Coverage {
        lines_total,
        lines_covered,
        percent: percent(lines_covered, lines_total),
        files,
    })
}

fn collect_clover_files(node: &XmlNode, base: &Path, out: &mut Vec<FileCoverage>) {
    for child in &node.children {
        if child.name == "file" {
            out.push(clover_file(child, base));
        } else {
            collect_clover_files(child, base, out);
        }
    }
}

fn clover_file(node: &XmlNode, base: &Path) -> FileCoverage {
    let mut total = 0;
    let mut covered = 0;
    for line in &node.children {
        if line.name != "line" {
            continue;
        }
        let kind = line.attr("type").unwrap_or("stmt");
        if kind != "stmt" && kind != "method" {
            continue;
        }
        total += 1;
        if line
            .attr("count")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0)
            > 0
        {
            covered += 1;
        }
    }
    let file = node
        .attr("name")
        .map(|name| display_relative(base, Path::new(name)))
        .unwrap_or_default();
    FileCoverage {
        file,
        total,
        covered,
        percent: percent(covered, total),
    }
}

fn metric_value(node: &XmlNode, key: &str) -> usize {
    node.attr(key)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
}

fn percent(covered: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        covered as f64 / total as f64 * 100.0
    }
}

fn collect_cases(node: &XmlNode, suite: &str, file: &str, base: &Path, out: &mut Vec<TestResult>) {
    let suite = if node.name == "testsuite" {
        node.attr("name")
            .filter(|name| !name.is_empty())
            .unwrap_or(suite)
    } else {
        suite
    };
    let file = node
        .attr("file")
        .filter(|file| !file.is_empty())
        .unwrap_or(file);

    for child in &node.children {
        match child.name.as_str() {
            "testsuite" => collect_cases(child, suite, file, base, out),
            "testcase" => out.push(extract_case(child, suite, file, base)),
            _ => {}
        }
    }
}

fn extract_case(case: &XmlNode, suite: &str, inherited_file: &str, base: &Path) -> TestResult {
    let name = case.attr("name").unwrap_or_default().to_string();
    let class = case
        .attr("class")
        .or_else(|| case.attr("classname"))
        .filter(|value| !value.is_empty())
        .unwrap_or(suite)
        .to_string();
    let file = case
        .attr("file")
        .filter(|value| !value.is_empty())
        .unwrap_or(inherited_file);
    let file = if file.is_empty() {
        String::new()
    } else {
        display_relative(base, Path::new(file))
    };
    let time = case
        .attr("time")
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0);

    let mut status = Status::Pass;
    let mut message = String::new();
    for child in &case.children {
        match child.name.as_str() {
            "error" => {
                status = Status::Error;
                message = failure_message(child);
            }
            "failure" => {
                if status != Status::Error {
                    status = Status::Fail;
                    message = failure_message(child);
                }
            }
            "skipped" if status == Status::Pass => {
                status = Status::Skip;
                message = child.text.trim().to_string();
            }
            _ => {}
        }
    }

    TestResult {
        name,
        class,
        file,
        time,
        status,
        message,
    }
}

fn failure_message(node: &XmlNode) -> String {
    let raw = node.text.trim();
    if raw.is_empty() {
        return String::new();
    }
    let first = raw.lines().next().unwrap_or_default().trim();
    let rest: Vec<&str> = if first.contains("::") {
        raw.lines().skip(1).collect()
    } else {
        raw.lines().collect()
    };
    let rest = rest.join("\n");
    let paragraph = rest.split("\n\n").next().unwrap_or_default().trim();
    if paragraph.is_empty() {
        first.to_string()
    } else {
        paragraph.to_string()
    }
}

pub fn has_failures(report: &Report) -> bool {
    report
        .tests
        .iter()
        .any(|test| matches!(test.status, Status::Fail | Status::Error))
}

pub fn tool_args(options: &RunOptions, junit: &Path, clover: Option<&Path>) -> Vec<String> {
    let mut args = vec![
        "--log-junit".to_string(),
        junit.to_string_lossy().into_owned(),
    ];
    if let Some(clover) = clover {
        args.push("--coverage-clover".to_string());
        args.push(clover.to_string_lossy().into_owned());
        if let Some(filter) = coverage_filter(&options.dir) {
            args.push("--coverage-filter".to_string());
            args.push(filter.to_string());
        }
    }
    if let Some(filter) = &options.filter {
        args.push("--filter".to_string());
        args.push(filter.clone());
    }
    if let Some(group) = &options.group {
        args.push("--group".to_string());
        args.push(group.clone());
    }
    if options.no_coverage {
        args.push("--no-coverage".to_string());
    }
    if let Some(file) = &options.file {
        args.push(file.clone());
    }
    args
}

/// PHPUnit 10+ needs a configured source filter before it will emit coverage.
/// When `phpunit.xml` has no `<source>` element, fall back to `src/`.
fn coverage_filter(dir: &Path) -> Option<&'static str> {
    let base = fs::canonicalize(dir).ok()?;
    if let Some((_, text)) = find_phpunit_config(&base) {
        if let Ok(root) = parse_xml(&text) {
            if find_named(&root, "source").is_some() {
                return None;
            }
        }
    }
    base.join("src").is_dir().then_some("src")
}

pub fn render_run(report: &Report) -> String {
    let mut output = String::new();
    for test in &report.tests {
        let label = match test.status {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::Error => "ERROR",
            Status::Skip => "SKIP",
        };
        let name = if test.class.is_empty() {
            test.name.clone()
        } else {
            format!("{}::{}", test.class, test.name)
        };
        if test.message.is_empty() {
            let _ = writeln!(output, "{label} {name} ({:.3}s)", test.time);
        } else {
            let message = test.message.replace('\n', " ");
            let _ = writeln!(output, "{label} {name} - {message}");
        }
    }
    let _ = writeln!(
        output,
        "Tests: {} | Passed: {} | Failed: {} | Errors: {} | Skipped: {} | Time: {:.3}s",
        report.summary.total,
        report.summary.passed,
        report.summary.failed,
        report.summary.errors,
        report.summary.skipped,
        report.summary.time
    );
    if let Some(coverage) = &report.coverage {
        let _ = writeln!(output);
        let _ = writeln!(output, "Coverage:");
        for file in &coverage.files {
            let _ = writeln!(
                output,
                "{}: {:.2}% ({}/{})",
                file.file, file.percent, file.covered, file.total
            );
        }
        let _ = writeln!(
            output,
            "Total: {:.2}% ({}/{})",
            coverage.percent, coverage.lines_covered, coverage.lines_total
        );
    }
    output
}

#[derive(Debug, Clone, Default)]
pub struct XmlNode {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<XmlNode>,
    pub text: String,
}

impl XmlNode {
    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }
}

/// Tiny XML parser sufficient for phpunit.xml and JUnit reports.
pub fn parse_xml(input: &str) -> Result<XmlNode> {
    let bytes = input.as_bytes();
    let mut position = 0;
    let mut stack: Vec<XmlNode> = Vec::new();
    let mut root = None;

    while position < bytes.len() {
        if bytes[position] != b'<' {
            let start = position;
            while position < bytes.len() && bytes[position] != b'<' {
                position += 1;
            }
            let chunk = &input[start..position];
            if let Some(node) = stack.last_mut() {
                node.text.push_str(&unescape_xml(chunk));
            }
            continue;
        }

        if input[position..].starts_with("<?") {
            let end = input[position + 2..]
                .find("?>")
                .context("unterminated XML declaration")?;
            position += 2 + end + 2;
        } else if input[position..].starts_with("<!--") {
            let end = input[position + 4..]
                .find("-->")
                .context("unterminated XML comment")?;
            position += 4 + end + 3;
        } else if input[position..].starts_with("<![CDATA[") {
            let end = input[position + 9..]
                .find("]]>")
                .context("unterminated CDATA section")?;
            if let Some(node) = stack.last_mut() {
                node.text.push_str(&input[position + 9..position + 9 + end]);
            }
            position += 9 + end + 3;
        } else if input[position..].starts_with("<!") {
            let end = input[position + 2..]
                .find('>')
                .context("unterminated declaration")?;
            position += 2 + end + 1;
        } else if input[position..].starts_with("</") {
            let end = input[position + 2..]
                .find('>')
                .context("unterminated closing tag")?;
            let name = input[position + 2..position + 2 + end].trim();
            let node = stack.pop().context("unexpected closing tag")?;
            if node.name != name {
                bail!(
                    "mismatched closing tag </{name}>, expected </{}>",
                    node.name
                );
            }
            attach_node(&mut stack, &mut root, node)?;
            position += 2 + end + 1;
        } else {
            let mut end = position + 1;
            let mut quote = None;
            while end < bytes.len() {
                let byte = bytes[end];
                if let Some(delimiter) = quote {
                    if byte == delimiter {
                        quote = None;
                    }
                } else if byte == b'\'' || byte == b'"' {
                    quote = Some(byte);
                } else if byte == b'>' {
                    break;
                }
                end += 1;
            }
            if end >= bytes.len() {
                bail!("unterminated XML tag");
            }

            let raw = input[position + 1..end].trim();
            let self_closing = raw.ends_with('/');
            let raw = raw.trim_end_matches('/').trim();
            let (name, attributes) = match raw.find(char::is_whitespace) {
                Some(index) => (&raw[..index], &raw[index..]),
                None => (raw, ""),
            };
            if name.is_empty() {
                bail!("empty XML tag name");
            }
            let node = XmlNode {
                name: name.to_string(),
                attrs: parse_attrs(attributes)?,
                children: Vec::new(),
                text: String::new(),
            };
            if self_closing {
                attach_node(&mut stack, &mut root, node)?;
            } else {
                stack.push(node);
            }
            position = end + 1;
        }
    }

    if let Some(node) = stack.last() {
        bail!("unclosed XML tag <{}>", node.name);
    }
    root.context("XML document has no root element")
}

fn attach_node(stack: &mut [XmlNode], root: &mut Option<XmlNode>, node: XmlNode) -> Result<()> {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(node);
    } else if root.is_none() {
        *root = Some(node);
    } else {
        bail!("XML document has multiple root elements");
    }
    Ok(())
}

fn parse_attrs(input: &str) -> Result<Vec<(String, String)>> {
    let mut attrs = Vec::new();
    let mut position = 0;
    let bytes = input.as_bytes();

    while position < bytes.len() {
        while position < bytes.len() && bytes[position].is_ascii_whitespace() {
            position += 1;
        }
        if position >= bytes.len() {
            break;
        }
        let key_start = position;
        while position < bytes.len()
            && !bytes[position].is_ascii_whitespace()
            && bytes[position] != b'='
        {
            position += 1;
        }
        let key = &input[key_start..position];
        while position < bytes.len() && bytes[position].is_ascii_whitespace() {
            position += 1;
        }
        if position >= bytes.len() || bytes[position] != b'=' {
            bail!("missing '=' after attribute '{key}'");
        }
        position += 1;
        while position < bytes.len() && bytes[position].is_ascii_whitespace() {
            position += 1;
        }
        if position >= bytes.len() || (bytes[position] != b'"' && bytes[position] != b'\'') {
            bail!("unquoted value for attribute '{key}'");
        }
        let quote = bytes[position];
        position += 1;
        let value_start = position;
        while position < bytes.len() && bytes[position] != quote {
            position += 1;
        }
        if position >= bytes.len() {
            bail!("unterminated value for attribute '{key}'");
        }
        let value = unescape_xml(&input[value_start..position]);
        attrs.push((key.to_string(), value));
        position += 1;
    }
    Ok(attrs)
}

fn unescape_xml(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(index) = rest.find('&') {
        output.push_str(&rest[..index]);
        let after = &rest[index + 1..];
        let Some(end) = after.find(';') else {
            output.push('&');
            rest = after;
            continue;
        };
        let entity = &after[..end];
        let replacement = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|value| u32::from_str_radix(value, 16).ok())
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|value| value.parse::<u32>().ok())
                })
                .and_then(char::from_u32),
        };
        match replacement {
            Some(character) => output.push(character),
            None => {
                output.push('&');
                output.push_str(entity);
                output.push(';');
            }
        }
        rest = &after[end + 1..];
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("php-test-unit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn parses_list_and_run_arguments() {
        assert_eq!(
            parse_args(["list", "--dir", "/tmp/project", "--json"]).unwrap(),
            Invocation::List(ListOptions {
                dir: PathBuf::from("/tmp/project"),
                json: true,
            })
        );

        assert_eq!(
            parse_args([
                "run",
                "--dir",
                "/tmp/project",
                "--filter",
                "Failing",
                "--file",
                "tests/FailingTest.php",
                "--group",
                "slow",
                "--json",
                "--no-coverage",
            ])
            .unwrap(),
            Invocation::Run(RunOptions {
                dir: PathBuf::from("/tmp/project"),
                filter: Some("Failing".to_string()),
                file: Some("tests/FailingTest.php".to_string()),
                group: Some("slow".to_string()),
                json: true,
                coverage: false,
                no_coverage: true,
            })
        );
        assert_eq!(parse_args(["--help"]).unwrap(), Invocation::Help);
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(parse_args(["nope"]).is_err());
        assert!(parse_args(["run", "--filter"]).is_err());
        assert!(parse_args(["list", "--filter", "x"]).is_err());
        assert!(parse_args(["run", "--wat"]).is_err());
    }

    #[test]
    fn parses_junit_statuses_and_messages() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="outer" tests="4">
    <testsuite name="App\Tests\SampleTest" file="/repo/tests/SampleTest.php">
      <testcase name="testPass" class="App\Tests\SampleTest" file="/repo/tests/SampleTest.php" time="0.001"/>
      <testcase name="testFail" class="App\Tests\SampleTest" file="/repo/tests/SampleTest.php" time="0.002">
        <failure type="ExpectationFailed">App\Tests\SampleTest::testFail
Failed asserting that false is true.

/repo/tests/SampleTest.php:12</failure>
      </testcase>
      <testcase name="testError" class="App\Tests\SampleTest" file="/repo/tests/SampleTest.php" time="0.003">
        <error type="RuntimeException">App\Tests\SampleTest::testError
RuntimeException: boom

/repo/tests/SampleTest.php:20</error>
      </testcase>
      <testcase name="testSkip" class="App\Tests\SampleTest" file="/repo/tests/SampleTest.php" time="0.004">
        <skipped>not today</skipped>
      </testcase>
    </testsuite>
  </testsuite>
</testsuites>"#;

        let dir = temp_dir("junit");
        let report = parse_junit(xml, &dir, Tool::Phpunit).unwrap();
        assert_eq!(report.tool, "phpunit");
        assert_eq!(report.summary.total, 4);
        assert_eq!(report.summary.passed, 1);
        assert_eq!(report.summary.failed, 1);
        assert_eq!(report.summary.errors, 1);
        assert_eq!(report.summary.skipped, 1);
        assert!((report.summary.time - 0.010).abs() < 1e-9);
        assert!(has_failures(&report));

        assert_eq!(report.tests[0].status, Status::Pass);
        assert_eq!(report.tests[1].status, Status::Fail);
        assert_eq!(
            report.tests[1].message,
            "Failed asserting that false is true."
        );
        assert_eq!(report.tests[2].status, Status::Error);
        assert_eq!(report.tests[2].message, "RuntimeException: boom");
        assert_eq!(report.tests[3].status, Status::Skip);
        assert_eq!(report.tests[3].message, "not today");
        assert!(report.tests[1].file.ends_with("tests/SampleTest.php"));
        assert_eq!(report.tests[1].class, "App\\Tests\\SampleTest");

        let json = serde_json::to_string(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["tool"], "phpunit");
        assert_eq!(value["tests"][1]["status"], "fail");
        assert_eq!(value["summary"]["errors"], 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn parses_xml_entities_cdata_and_attributes() {
        let node =
            parse_xml(r#"<root a="1&amp;2"><child><![CDATA[a < b & c]]></child><empty/></root>"#)
                .unwrap();
        assert_eq!(node.attr("a"), Some("1&2"));
        assert_eq!(node.children[0].text, "a < b & c");
        assert_eq!(node.children[1].name, "empty");
    }

    #[test]
    fn discovers_test_files_from_tests_and_config() {
        let dir = temp_dir("discovery");
        fs::create_dir_all(dir.join("tests/Nested")).unwrap();
        fs::create_dir_all(dir.join("spec")).unwrap();
        fs::write(dir.join("tests/AlphaTest.php"), "<?php").unwrap();
        fs::write(dir.join("tests/Nested/BetaTest.php"), "<?php").unwrap();
        fs::write(dir.join("tests/Helper.php"), "<?php").unwrap();
        fs::write(dir.join("spec/GammaTest.php"), "<?php").unwrap();
        fs::write(
            dir.join("phpunit.xml"),
            r#"<phpunit><coverage><include><directory>src</directory></include></coverage>
            <testsuites><testsuite name="spec"><directory>spec</directory>
            <file>tests/AlphaTest.php</file></testsuite></testsuites></phpunit>"#,
        )
        .unwrap();

        let files = list_test_files(&dir).unwrap();
        assert_eq!(
            files,
            vec![
                "spec/GammaTest.php",
                "tests/AlphaTest.php",
                "tests/Nested/BetaTest.php",
            ]
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn finds_tools_walking_up() {
        let dir = temp_dir("find-tool");
        fs::create_dir_all(dir.join("vendor/bin")).unwrap();
        fs::create_dir_all(dir.join("nested/deeper")).unwrap();
        fs::write(dir.join("vendor/bin/phpunit"), "#!/bin/sh\n").unwrap();

        let (tool, path) = find_tool(&dir.join("nested/deeper")).unwrap().unwrap();
        assert_eq!(tool, Tool::Phpunit);
        assert!(path.ends_with("vendor/bin/phpunit"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn builds_tool_arguments() {
        let options = RunOptions {
            dir: PathBuf::from("."),
            filter: Some("Failing".to_string()),
            file: Some("tests/FailingTest.php".to_string()),
            group: Some("slow".to_string()),
            json: true,
            coverage: false,
            no_coverage: true,
        };
        assert_eq!(
            tool_args(&options, Path::new("/tmp/junit.xml"), None),
            vec![
                "--log-junit",
                "/tmp/junit.xml",
                "--filter",
                "Failing",
                "--group",
                "slow",
                "--no-coverage",
                "tests/FailingTest.php",
            ]
        );
    }

    fn clover_sample(base: &Path) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<coverage generated="1">
  <project timestamp="1" name="Clover Coverage">
    <package name="App">
      <file name="{base}/src/Calculator.php">
        <class name="App\Calculator"/>
        <line num="9" type="method" name="add" count="1"/>
        <line num="11" type="stmt" count="1"/>
        <line num="14" type="method" name="subtract" count="0"/>
        <line num="16" type="stmt" count="0"/>
      </file>
      <file name="{base}/src/Helper.php">
        <line num="5" type="stmt" count="1"/>
        <line num="6" type="stmt" count="1"/>
      </file>
    </package>
    <metrics statements="4" coveredstatements="2"/>
  </project>
</coverage>"#,
            base = base.display()
        )
    }

    #[test]
    fn parses_clover_statements_methods_and_percentages() {
        let dir = temp_dir("clover");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/Calculator.php"), "<?php").unwrap();
        fs::write(dir.join("src/Helper.php"), "<?php").unwrap();
        let base = fs::canonicalize(&dir).unwrap();

        let coverage = parse_clover(&clover_sample(&base), &dir).unwrap();
        assert_eq!(coverage.files.len(), 2);
        assert_eq!(coverage.files[0].file, "src/Calculator.php");
        assert_eq!(coverage.files[0].total, 4);
        assert_eq!(coverage.files[0].covered, 2);
        assert!((coverage.files[0].percent - 50.0).abs() < 1e-9);
        assert_eq!(coverage.files[1].file, "src/Helper.php");
        assert_eq!(coverage.files[1].total, 2);
        assert_eq!(coverage.files[1].covered, 2);
        assert!((coverage.files[1].percent - 100.0).abs() < 1e-9);
        assert_eq!(coverage.lines_total, 6);
        assert_eq!(coverage.lines_covered, 4);
        assert!((coverage.percent - 200.0 / 3.0).abs() < 1e-9);

        let json = serde_json::to_value(&coverage).unwrap();
        assert_eq!(json["lines_total"], 6);
        assert_eq!(json["lines_covered"], 4);
        assert_eq!(json["files"][0]["file"], "src/Calculator.php");
        assert_eq!(json["files"][0]["total"], 4);
        assert_eq!(json["files"][0]["covered"], 2);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn handles_empty_and_missing_clover_coverage() {
        let dir = temp_dir("clover-empty");

        let empty = parse_clover("<coverage><project/></coverage>", &dir).unwrap();
        assert_eq!(empty.lines_total, 0);
        assert_eq!(empty.lines_covered, 0);
        assert_eq!(empty.percent, 0.0);
        assert!(empty.files.is_empty());

        let metrics = parse_clover(
            r#"<coverage><project><metrics statements="10" coveredstatements="5"/></project></coverage>"#,
            &dir,
        )
        .unwrap();
        assert_eq!(metrics.lines_total, 10);
        assert_eq!(metrics.lines_covered, 5);
        assert!((metrics.percent - 50.0).abs() < 1e-9);

        assert!(parse_clover("", &dir).is_err());
        assert!(parse_clover("<coverage/>", &dir).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn computes_percent_without_division_by_zero() {
        assert_eq!(percent(0, 0), 0.0);
        assert_eq!(percent(5, 5), 100.0);
        assert!((percent(1, 3) - 33.333_333).abs() < 1e-5);
    }

    #[test]
    fn builds_coverage_arguments_with_source_fallback() {
        let dir = temp_dir("coverage-args");
        fs::create_dir_all(dir.join("src")).unwrap();
        let options = RunOptions {
            dir: dir.clone(),
            coverage: true,
            ..RunOptions::default()
        };
        let args = tool_args(
            &options,
            Path::new("/tmp/junit.xml"),
            Some(Path::new("/tmp/clover.xml")),
        );
        assert!(args.contains(&"--coverage-clover".to_string()));
        assert!(args.contains(&"/tmp/clover.xml".to_string()));
        assert!(args.contains(&"--coverage-filter".to_string()));
        assert!(args.contains(&"src".to_string()));
        assert!(!args.contains(&"--no-coverage".to_string()));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn renders_coverage_after_test_summary() {
        let report = Report {
            tool: "phpunit".to_string(),
            tests: Vec::new(),
            summary: Summary {
                total: 0,
                passed: 0,
                failed: 0,
                errors: 0,
                skipped: 0,
                time: 0.0,
            },
            coverage: Some(Coverage {
                lines_total: 4,
                lines_covered: 3,
                percent: 75.0,
                files: vec![FileCoverage {
                    file: "src/Calculator.php".to_string(),
                    total: 4,
                    covered: 3,
                    percent: 75.0,
                }],
            }),
        };
        let text = render_run(&report);
        assert!(text.contains("Coverage:"));
        assert!(text.contains("src/Calculator.php: 75.00% (3/4)"));
        assert!(text.contains("Total: 75.00% (3/4)"));
    }
}
