use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

mod postman;

#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Raw(String),
    UrlEncoded(Vec<(String, String)>),
    FormData(Vec<FormField>),
    GraphQL { query: String, variables: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FormField {
    pub key: String,
    pub value: String,
    pub is_file: bool,
}

#[derive(Debug, Clone)]
struct Request {
    name: String,
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Body>,
    body_dir: PathBuf,
}

#[derive(Debug)]
struct Document {
    vars: HashMap<String, String>,
    requests: Vec<Request>,
}

struct ExecResult {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
    time_ms: u128,
    size: usize,
}

fn usage() -> &'static str {
    "usage:\n  php-http list <file>\n  php-http run <file> --name <request> [--var key=value]... [--env file] [--json] [--follow] [--history <file>]\n  php-http run <file> --all [--var key=value]... [--env file] [--json] [--follow] [--history <file>]\n  php-http collection list <collection.json>\n  php-http collection env <collection.json> --env <environment.json> [--var key=value]...\n  php-http collection run <collection.json> --request <name|id> [--env <environment.json>] [--var key=value]... [--json] [--follow] [--history <file>]\n  php-http collection run <collection.json> --all [--env <environment.json>] [--var key=value]... [--json] [--fail-fast] [--follow] [--history <file>]"
}

fn parse_var_line(line: &str) -> Option<(String, String)> {
    let t = line.trim();
    let rest = t.strip_prefix('@')?;
    let (key, value) = rest.split_once('=')?;
    let key = key.trim();
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return None;
    }
    Some((key.to_string(), value.trim().to_string()))
}

fn parse_document(content: &str) -> Document {
    let mut vars = HashMap::new();
    for line in content.lines() {
        if let Some((k, v)) = parse_var_line(line) {
            vars.insert(k, v);
        }
    }

    let mut segments: Vec<(String, Vec<&str>)> = Vec::new();
    let mut name = String::new();
    let mut lines: Vec<&str> = Vec::new();
    for line in content.lines() {
        if line.trim_start().starts_with("###") {
            if !lines.is_empty() || !name.is_empty() {
                segments.push((name.clone(), lines.clone()));
            }
            name = line.trim_start().trim_start_matches('#').trim().to_string();
            lines.clear();
        } else {
            lines.push(line);
        }
    }
    if !lines.is_empty() || !name.is_empty() {
        segments.push((name, lines));
    }

    let mut requests = Vec::new();
    for (section_name, section_lines) in segments {
        if let Some(req) = parse_request(&section_name, &section_lines, requests.len()) {
            requests.push(req);
        }
    }

    Document { vars, requests }
}

fn parse_request(name: &str, lines: &[&str], index: usize) -> Option<Request> {
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        if t.is_empty()
            || t.starts_with('#')
            || t.starts_with("//")
            || parse_var_line(lines[i]).is_some()
        {
            i += 1;
            continue;
        }
        break;
    }
    if i >= lines.len() {
        return None;
    }

    let request_line = lines[i].trim();
    i += 1;
    let mut parts = request_line.split_whitespace().collect::<Vec<_>>();
    if parts.len() >= 2 && parts.last().is_some_and(|p| p.starts_with("HTTP/")) {
        parts.pop();
    }
    let (method, url) = match parts.as_slice() {
        [url] => ("GET".to_string(), (*url).to_string()),
        [method, url] if is_method(method) => (method.to_ascii_uppercase(), (*url).to_string()),
        [method, url, ..] => (method.to_ascii_uppercase(), (*url).to_string()),
        _ => return None,
    };

    let mut headers = Vec::new();
    while i < lines.len() {
        let t = lines[i].trim();
        if t.is_empty() {
            i += 1;
            break;
        }
        if t.starts_with('#') || t.starts_with("//") {
            i += 1;
            continue;
        }
        match t.split_once(':') {
            Some((key, value)) if !key.trim().is_empty() && !key.contains(char::is_whitespace) => {
                headers.push((key.trim().to_string(), value.trim().to_string()));
                i += 1;
            }
            _ => break,
        }
    }

    let body_text = lines[i..].join("\n");
    let body_trimmed = body_text.trim_end();
    let body = if body_trimmed.is_empty() {
        None
    } else {
        Some(Body::Raw(body_trimmed.to_string()))
    };

    let name = if name.is_empty() {
        format!("{}-{}", method, index + 1)
    } else {
        name.to_string()
    };

    Some(Request {
        name,
        method,
        url,
        headers,
        body,
        body_dir: PathBuf::from("."),
    })
}

fn is_method(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 12
        && token.chars().all(|c| c.is_ascii_alphabetic())
        && token == token.to_ascii_uppercase()
}

fn interpolate(input: &str, vars: &HashMap<String, String>) -> String {
    let mut out = input.to_string();
    for _ in 0..10 {
        let mut changed = false;
        let mut result = String::new();
        let mut rest = out.as_str();
        while let Some(start) = rest.find("{{") {
            result.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            match after.find("}}") {
                Some(end) => {
                    let key = after[..end].trim();
                    if let Some(value) = vars.get(key) {
                        result.push_str(value);
                        changed = true;
                    } else {
                        result.push_str("{{");
                        result.push_str(&after[..end]);
                        result.push_str("}}");
                    }
                    rest = &after[end + 2..];
                }
                None => {
                    result.push_str(&rest[start..]);
                    rest = "";
                }
            }
        }
        result.push_str(rest);
        out = result;
        if !changed {
            break;
        }
    }
    out
}

fn unresolved_var(url: &str) -> Option<String> {
    let start = url.find("{{")?;
    let end = url[start + 2..].find("}}").map(|e| start + 2 + e);
    Some(match end {
        Some(end) => url[start..end + 2].to_string(),
        None => url[start..].to_string(),
    })
}

fn load_document(path: &Path) -> Result<Document> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read .http file: {}", path.display()))?;
    let mut doc = parse_document(&content);
    let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    for req in &mut doc.requests {
        req.body_dir = base.clone();
    }
    Ok(doc)
}

fn apply_env(vars: &mut HashMap<String, String>, path: &Path) -> Result<()> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read env file: {}", path.display()))?;
    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = t.split_once('=') {
            vars.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    Ok(())
}

fn temp_paths() -> (PathBuf, PathBuf, PathBuf) {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("php-http-{}-{}", std::process::id(), nanos));
    (dir.clone(), dir.join("headers"), dir.join("body"))
}

fn body_args(
    body: &Body,
    vars: &HashMap<String, String>,
    base_dir: &Path,
) -> Result<Vec<(String, String)>> {
    let mut args = Vec::new();
    match body {
        Body::Raw(raw) => {
            args.push(("--data-binary".to_string(), interpolate(raw, vars)));
        }
        Body::UrlEncoded(fields) => {
            let encoded = fields
                .iter()
                .map(|(key, value)| {
                    format!(
                        "{}={}",
                        form_encode(&interpolate(key, vars)),
                        form_encode(&interpolate(value, vars))
                    )
                })
                .collect::<Vec<_>>()
                .join("&");
            args.push(("--data-binary".to_string(), encoded));
        }
        Body::FormData(fields) => {
            for field in fields {
                let key = interpolate(&field.key, vars);
                if field.is_file {
                    let src = interpolate(&field.value, vars);
                    let path = base_dir.join(&src);
                    if !path.is_file() {
                        bail!(
                            "form-data file not found for field '{}': {}",
                            key,
                            path.display()
                        );
                    }
                    let file_arg = format!("@{}", path.display());
                    args.push((
                        "--form".to_string(),
                        format!("{}={}", key, file_arg),
                    ));
                } else {
                    args.push((
                        "--form".to_string(),
                        format!("{}={}", key, interpolate(&field.value, vars)),
                    ));
                }
            }
        }
        Body::GraphQL { query, variables } => {
            let raw = interpolate(variables, vars);
            let variables = if raw.trim().is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&raw).unwrap_or_else(|_| Value::String(raw.clone()))
            };
            let payload = json!({
                "query": interpolate(query, vars),
                "variables": variables,
            });
            args.push((
                "--data-binary".to_string(),
                serde_json::to_string(&payload)?,
            ));
        }
    }
    Ok(args)
}

fn form_encode(input: &str) -> String {
    let mut out = String::new();
    for byte in input.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn first_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

pub(crate) fn append_history(
    path: &Path,
    collection: &str,
    request_id: &str,
    request_name: &str,
    method: &str,
    url: &str,
    result: &ExecResult,
) -> Result<()> {
    let entry = json!({
        "timestamp": unix_timestamp(),
        "collection": collection,
        "request_id": request_id,
        "request_name": request_name,
        "method": method,
        "url": url,
        "status": result.status,
        "time_ms": result.time_ms,
        "size": result.size,
        "body": first_chars(&result.body, 500),
    });
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open history file: {}", path.display()))?;
    writeln!(file, "{}", serde_json::to_string(&entry)?)
        .with_context(|| format!("failed to write history file: {}", path.display()))?;
    Ok(())
}

fn execute(req: &Request, vars: &HashMap<String, String>, follow: bool) -> Result<ExecResult> {
    let url = interpolate(&req.url, vars);
    if let Some(var) = unresolved_var(&url) {
        bail!(
            "unresolved variable {} in URL for request '{}': {}",
            var,
            req.name,
            url
        );
    }

    let (dir, headers_path, body_path) = temp_paths();
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create temp dir: {}", dir.display()))?;

    let mut cursor = Command::new("curl");
    cursor
        .arg("--silent")
        .arg("--show-error")
        .arg("--max-time")
        .arg("30")
        .arg("--request")
        .arg(&req.method)
        .arg("--dump-header")
        .arg(&headers_path)
        .arg("--output")
        .arg(&body_path)
        .arg("--write-out")
        .arg("%{http_code}|%{time_total}");
    if follow {
        cursor.arg("--location");
    }
    for (name, value) in &req.headers {
        cursor
            .arg("--header")
            .arg(format!("{}: {}", name, interpolate(value, vars)));
    }
    if let Some(body) = &req.body {
        for (flag, value) in body_args(body, vars, &req.body_dir)? {
            cursor.arg(flag).arg(value);
        }
    }
    cursor.arg(&url);

    let output = cursor
        .output()
        .context("failed to run curl (is it installed?)")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = fs::remove_dir_all(&dir);
        bail!(
            "curl failed for request '{}' (exit {:?}): {}",
            req.name,
            output.status.code(),
            stderr.trim()
        );
    }

    let headers_raw = fs::read(&headers_path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let body_bytes = fs::read(&body_path).unwrap_or_default();
    let size = body_bytes.len();
    let body = String::from_utf8_lossy(&body_bytes).into_owned();
    let _ = fs::remove_dir_all(&dir);

    let write_out = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let mut parts = write_out.split('|');
    let mut status = parts.next().unwrap_or("").parse::<u16>().unwrap_or(0);
    let time_ms = parts
        .next()
        .and_then(|seconds| seconds.parse::<f64>().ok())
        .map(|seconds| (seconds * 1000.0).round() as u128)
        .unwrap_or(0);
    let (parsed_status, headers) = parse_response_headers(&headers_raw);
    if status == 0 {
        status = parsed_status;
    }

    Ok(ExecResult {
        status,
        headers,
        body,
        time_ms,
        size,
    })
}

fn parse_response_headers(raw: &str) -> (u16, Vec<(String, String)>) {
    let normalized = raw.replace("\r\n", "\n");
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in normalized.split('\n') {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        blocks.push(current);
    }

    let Some(block) = blocks
        .iter()
        .rev()
        .find(|b| b.first().is_some_and(|l| l.starts_with("HTTP/")))
    else {
        return (0, Vec::new());
    };

    let status = block
        .first()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);

    let mut headers: Vec<(String, String)> = Vec::new();
    for line in &block[1..] {
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(last) = headers.last_mut() {
                last.1.push(' ');
                last.1.push_str(line.trim());
            }
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    (status, headers)
}

fn print_human(result: &ExecResult) {
    println!("HTTP {}", result.status);
    for (name, value) in &result.headers {
        println!("{}: {}", name, value);
    }
    println!();
    print!("{}", result.body);
    if !result.body.ends_with('\n') {
        println!();
    }
}

fn run_one(
    req: &Request,
    vars: &HashMap<String, String>,
    follow: bool,
    as_json: bool,
    history: Option<&Path>,
    source: &str,
) -> Result<()> {
    let result = execute(req, vars, follow)?;
    if let Some(path) = history {
        append_history(
            path,
            source,
            &req.name,
            &req.name,
            &req.method,
            &interpolate(&req.url, vars),
            &result,
        )?;
    }
    if as_json {
        let mut headers = Map::new();
        for (name, value) in &result.headers {
            if let Some(Value::String(existing)) = headers.get(name) {
                headers.insert(
                    name.clone(),
                    Value::String(format!("{}, {}", existing, value)),
                );
            } else {
                headers.insert(name.clone(), Value::String(value.clone()));
            }
        }
        let value = json!({
            "name": req.name,
            "method": req.method,
            "url": interpolate(&req.url, vars),
            "status": result.status,
            "headers": Value::Object(headers),
            "body": result.body,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        print_human(&result);
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        bail!("{}\n", usage());
    }

    match args[0].as_str() {
        "collection" => return postman::run_cli(&args[1..]),
        "list" => {
            let file = args.get(1).context("list requires a .http file")?;
            let doc = load_document(Path::new(file))?;
            for req in &doc.requests {
                println!("{}\t{}\t{}", req.name, req.method, req.url);
            }
        }
        "run" => {
            let file = args.get(1).context("run requires a .http file")?;
            let mut name: Option<String> = None;
            let mut all = false;
            let mut as_json = false;
            let mut follow = false;
            let mut env_file: Option<String> = None;
            let mut overrides: Vec<(String, String)> = Vec::new();
            let mut history: Option<PathBuf> = None;

            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--name" => {
                        i += 1;
                        name = Some(args.get(i).context("--name requires a value")?.clone());
                    }
                    "--all" => all = true,
                    "--json" => as_json = true,
                    "--follow" => follow = true,
                    "--history" => {
                        i += 1;
                        history = Some(PathBuf::from(
                            args.get(i).context("--history requires a file")?,
                        ));
                    }
                    "--env" => {
                        i += 1;
                        env_file = Some(args.get(i).context("--env requires a file")?.clone());
                    }
                    "--var" => {
                        i += 1;
                        let raw = args.get(i).context("--var requires key=value")?;
                        let (key, value) = raw.split_once('=').with_context(|| {
                            format!("invalid --var (expected key=value): {}", raw)
                        })?;
                        overrides.push((key.trim().to_string(), value.trim().to_string()));
                    }
                    other => bail!("unknown run option: {}\n\n{}", other, usage()),
                }
                i += 1;
            }

            if !all && name.is_none() {
                bail!("run requires --name <request> or --all\n\n{}", usage());
            }
            if all && name.is_some() {
                bail!("use either --name or --all, not both");
            }

            let doc = load_document(Path::new(file))?;
            let mut vars = doc.vars.clone();
            if let Some(env) = &env_file {
                apply_env(&mut vars, Path::new(env))?;
            }
            for (key, value) in overrides {
                vars.insert(key, value);
            }

            if all {
                if as_json {
                    let mut results = Vec::new();
                    for req in &doc.requests {
                        let result = execute(req, &vars, follow)?;
                        if let Some(path) = history.as_deref() {
                            append_history(
                                path,
                                file,
                                &req.name,
                                &req.name,
                                &req.method,
                                &interpolate(&req.url, &vars),
                                &result,
                            )?;
                        }
                        let mut headers = Map::new();
                        for (hname, hvalue) in &result.headers {
                            if let Some(Value::String(existing)) = headers.get(hname) {
                                headers.insert(
                                    hname.clone(),
                                    Value::String(format!("{}, {}", existing, hvalue)),
                                );
                            } else {
                                headers.insert(hname.clone(), Value::String(hvalue.clone()));
                            }
                        }
                        results.push(json!({
                            "name": req.name,
                            "method": req.method,
                            "url": interpolate(&req.url, &vars),
                            "status": result.status,
                            "headers": Value::Object(headers),
                            "body": result.body,
                        }));
                    }
                    println!("{}", serde_json::to_string_pretty(&results)?);
                } else {
                    for (index, req) in doc.requests.iter().enumerate() {
                        if index > 0 {
                            println!();
                        }
                        println!("=== {} ===", req.name);
                        run_one(req, &vars, follow, false, history.as_deref(), file)?;
                    }
                }
            } else {
                let wanted = name.unwrap();
                let req = doc.requests.iter().find(|r| r.name == wanted);
                match req {
                    Some(req) => {
                        run_one(req, &vars, follow, as_json, history.as_deref(), file)?
                    }
                    None => {
                        let available = doc
                            .requests
                            .iter()
                            .map(|r| r.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        bail!("request '{}' not found (available: {})", wanted, available);
                    }
                }
            }
        }
        other => bail!("unknown command: {}\n\n{}", other, usage()),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_encodes_spaces_and_specials() {
        assert_eq!(form_encode("a b+c&d=e"), "a+b%2Bc%26d%3De");
    }

    #[test]
    fn builds_urlencoded_body_with_interpolation() {
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), "Ada Lovelace".to_string());
        let body = Body::UrlEncoded(vec![
            ("full name".to_string(), "{{name}}".to_string()),
            ("note".to_string(), "a+b".to_string()),
        ]);
        let args = body_args(&body, &vars, Path::new(".")).expect("body args");
        assert_eq!(
            args,
            vec![(
                "--data-binary".to_string(),
                "full+name=Ada+Lovelace&note=a%2Bb".to_string()
            )]
        );
    }

    #[test]
    fn builds_formdata_args_and_checks_files() {
        let dir =
            std::env::temp_dir().join(format!("php-http-formdata-test-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        fs::write(dir.join("upload.txt"), "hello upload\n").expect("write upload");
        let body = Body::FormData(vec![
            FormField {
                key: "title".to_string(),
                value: "hello".to_string(),
                is_file: false,
            },
            FormField {
                key: "upload".to_string(),
                value: "upload.txt".to_string(),
                is_file: true,
            },
        ]);
        let args = body_args(&body, &HashMap::new(), &dir).expect("body args");
        assert_eq!(args[0], ("--form".to_string(), "title=hello".to_string()));
        assert_eq!(
            args[1],
            (
                "--form".to_string(),
                format!("upload=@{}", dir.join("upload.txt").display())
            )
        );

        let missing = Body::FormData(vec![FormField {
            key: "upload".to_string(),
            value: "nope.txt".to_string(),
            is_file: true,
        }]);
        let err = body_args(&missing, &HashMap::new(), &dir).expect_err("missing file fails");
        assert!(err.to_string().contains("form-data file not found"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn builds_graphql_json_body() {
        let body = Body::GraphQL {
            query: "query { x }".to_string(),
            variables: "{\"id\": 7}".to_string(),
        };
        let args = body_args(&body, &HashMap::new(), Path::new(".")).expect("body args");
        assert_eq!(
            args,
            vec![(
                "--data-binary".to_string(),
                "{\"query\":\"query { x }\",\"variables\":{\"id\":7}}".to_string()
            )]
        );
    }

    #[test]
    fn appends_one_json_line_per_result() {
        let dir = std::env::temp_dir().join(format!("php-http-history-test-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("history.jsonl");
        let result = ExecResult {
            status: 200,
            headers: Vec::new(),
            body: "x".repeat(600),
            time_ms: 5,
            size: 600,
        };
        append_history(&path, "Demo", "id-1", "Name", "POST", "http://x", &result)
            .expect("append history");
        append_history(&path, "Demo", "id-2", "Other", "GET", "http://x", &result)
            .expect("append history");

        let content = fs::read_to_string(&path).expect("read history");
        let lines = content.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        let value: Value = serde_json::from_str(lines[0]).expect("json line");
        assert_eq!(value["collection"], "Demo");
        assert_eq!(value["method"], "POST");
        assert_eq!(value["status"], 200);
        assert_eq!(value["body"].as_str().unwrap().chars().count(), 500);

        let _ = fs::remove_dir_all(&dir);
    }
}
