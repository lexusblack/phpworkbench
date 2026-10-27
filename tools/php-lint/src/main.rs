use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use php_lint::{
    analyse_path, human_exit_code, normalize, normalize_psalm, parse_args, render_human,
    requested_tool, resolve_phpstan, resolve_psalm, resolve_tool, validate_dir, CheckOptions,
    Invocation, Report, Tool,
};
use serde_json::{json, Value};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(&args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("php-lint: {error:#}");
            2
        }
    };
    std::process::exit(code);
}

fn run(args: &[String]) -> Result<i32> {
    if args.iter().any(|arg| arg == "--lsp") {
        let tool = parse_lsp_tool(args)?;
        run_lsp(tool)?;
        return Ok(0);
    }
    match parse_args(args.iter().cloned())? {
        Invocation::Help => {
            print!("{}", php_lint::USAGE);
            Ok(0)
        }
        Invocation::Check(options) => check(&options),
    }
}

fn parse_lsp_tool(args: &[String]) -> Result<Option<Tool>> {
    let mut tool = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--lsp" => {}
            "--tool" => {
                let value = iter.next().with_context(|| "missing value for --tool")?;
                tool = Some(Tool::parse(value)?);
            }
            other => bail!("unknown option '{other}' with --lsp\n\n{}", php_lint::USAGE),
        }
    }
    Ok(tool)
}

fn check(options: &CheckOptions) -> Result<i32> {
    let report = analyse(options)?;

    if options.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        print!("{}", render_human(&report));
    }

    Ok(human_exit_code(&report))
}

fn analyse(options: &CheckOptions) -> Result<Report> {
    validate_dir(&options.dir)?;
    validate_file(options)?;

    match resolve_tool(options)? {
        Tool::Phpstan => analyse_phpstan(options),
        Tool::Psalm => analyse_psalm(options),
        Tool::Auto => unreachable!("auto is resolved to a concrete tool"),
    }
}

fn analyse_phpstan(options: &CheckOptions) -> Result<Report> {
    let phpstan = resolve_phpstan(options)?;

    let mut command = Command::new(&phpstan);
    command
        .current_dir(&options.dir)
        .arg("analyse")
        .arg(analyse_path(options))
        .arg("--error-format=json")
        .arg("--no-progress")
        .arg("--no-interaction")
        .arg(format!("--memory-limit={}", options.memory))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(level) = options.level {
        command.arg("--level").arg(level.to_string());
    }

    let output = command.output().with_context(|| {
        format!(
            "failed to run PHPStan at {}.\n\
             Install it with: composer require --dev phpstan/phpstan",
            phpstan.display()
        )
    })?;

    let exit_code = output.status.code().unwrap_or(2);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    match normalize(&stdout, exit_code) {
        Ok(report) => Ok(report),
        Err(error) => {
            let trimmed = stderr.trim();
            if trimmed.is_empty() {
                bail!("{error}");
            }
            bail!("{error}\nPHPStan stderr:\n{trimmed}")
        }
    }
}

fn analyse_psalm(options: &CheckOptions) -> Result<Report> {
    let psalm = resolve_psalm(options)?;

    let output = Command::new(&psalm)
        .current_dir(&options.dir)
        .arg("--output-format=json")
        .arg("--no-progress")
        .arg("--no-cache")
        .arg(format!("--memory-limit={}", options.memory))
        .arg(analyse_path(options))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| {
            format!(
                "failed to run Psalm at {}.\n\
                 Install it with: composer require --dev vimeo/psalm",
                psalm.display()
            )
        })?;

    let exit_code = output.status.code().unwrap_or(2);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    match normalize_psalm(&stdout, exit_code) {
        Ok(report) => Ok(report),
        Err(error) => {
            let trimmed = stderr.trim();
            if trimmed.is_empty() {
                bail!("{error}");
            }
            bail!("{error}\nPsalm stderr:\n{trimmed}")
        }
    }
}

fn run_lsp(requested: Option<Tool>) -> Result<()> {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();

    while let Some(message) = read_message(&mut reader)? {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => {
                write_message(
                    &mut writer,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "capabilities": { "textDocumentSync": 1 },
                            "serverInfo": {
                                "name": "php-lint",
                                "version": env!("CARGO_PKG_VERSION"),
                            },
                        },
                    }),
                )?;
            }
            "initialized" => {}
            "shutdown" => {
                write_message(
                    &mut writer,
                    &json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null }),
                )?;
            }
            "exit" => break,
            "textDocument/didOpen" | "textDocument/didSave" => {
                if let Err(error) = publish_diagnostics(&mut writer, &params, requested) {
                    eprintln!("php-lint lsp: {error:#}");
                }
            }
            _ => {
                if let Some(id) = id {
                    write_message(
                        &mut writer,
                        &json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null }),
                    )?;
                }
            }
        }
    }

    Ok(())
}

fn read_message<R: BufRead>(reader: &mut R) -> Result<Option<Value>> {
    let mut content_length = None;

    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = value.trim().parse::<usize>().ok();
        }
    }

    let Some(length) = content_length else {
        return Ok(None);
    };
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body)?))
}

fn write_message<W: Write>(writer: &mut W, message: &Value) -> Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

fn publish_diagnostics<W: Write>(
    writer: &mut W,
    params: &Value,
    requested: Option<Tool>,
) -> Result<()> {
    let Some(uri) = params
        .get("textDocument")
        .and_then(|document| document.get("uri"))
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    let Some(path) = uri_to_path(uri) else {
        return Ok(());
    };
    if path.extension().and_then(|extension| extension.to_str()) != Some("php") {
        return Ok(());
    }

    let requested = requested_tool(requested, false)?;
    let Some(dir) = find_project_dir(&path, requested) else {
        eprintln!(
            "php-lint lsp: no phpstan.neon, phpstan.neon.dist, psalm.xml, or psalm.xml.dist found for {}",
            path.display()
        );
        return Ok(());
    };

    let tool = match php_lint::select_tool(requested, &dir) {
        Ok(tool) => tool,
        Err(error) => {
            eprintln!("php-lint lsp: {error:#}");
            return Ok(());
        }
    };

    let options = CheckOptions {
        dir,
        file: Some(path.clone()),
        level: None,
        json: false,
        memory: "2G".to_string(),
        phpstan: None,
        tool: Some(tool),
    };

    let report = match analyse(&options) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("php-lint lsp: {error:#}");
            return Ok(());
        }
    };

    let lines = read_lines(&path);
    let diagnostics = report
        .errors
        .iter()
        .map(|error| {
            let line = error.line.saturating_sub(1);
            let end = lines
                .get(line as usize)
                .map(|text| text.chars().count())
                .unwrap_or(0);
            let mut diagnostic = json!({
                "range": {
                    "start": { "line": line, "character": 0 },
                    "end": { "line": line, "character": end },
                },
                "severity": lsp_severity(&error.severity),
                "source": report.tool,
                "message": error.message,
            });
            if let Some(identifier) = &error.identifier {
                diagnostic["code"] = json!(identifier);
            }
            diagnostic
        })
        .collect::<Vec<_>>();

    write_message(
        writer,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": diagnostics },
        }),
    )
}

fn lsp_severity(severity: &str) -> u8 {
    match severity {
        "warning" => 2,
        "info" => 3,
        _ => 1,
    }
}

fn read_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

fn find_project_dir(file: &Path, requested: Tool) -> Option<PathBuf> {
    let configs: &[&str] = match requested {
        Tool::Phpstan => &["phpstan.neon", "phpstan.neon.dist"],
        Tool::Psalm => &["psalm.xml", "psalm.xml.dist"],
        Tool::Auto => &[
            "phpstan.neon",
            "phpstan.neon.dist",
            "psalm.xml",
            "psalm.xml.dist",
        ],
    };
    let mut dir = file.parent()?.to_path_buf();
    loop {
        if configs.iter().any(|config| dir.join(config).is_file()) {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let encoded = uri.strip_prefix("file://")?;
    Some(PathBuf::from(percent_decode(encoded)))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&input[index + 1..index + 3], 16) {
                output.push(value);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }

    String::from_utf8_lossy(&output).into_owned()
}

fn validate_file(options: &CheckOptions) -> Result<()> {
    let Some(file) = &options.file else {
        return Ok(());
    };
    let candidate = if file.is_absolute() {
        file.clone()
    } else {
        options.dir.join(file)
    };
    if !candidate.is_file() {
        bail!("target file does not exist: {}", candidate.display());
    }
    Ok(())
}
