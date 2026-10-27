use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::{
    append_history, execute, interpolate, unresolved_var, Body, ExecResult, FormField, Request,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyLocation {
    Header,
    Query,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Auth {
    Bearer(String),
    Basic { username: String, password: String },
    ApiKey {
        key: String,
        value: String,
        location: ApiKeyLocation,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct RequestSpec {
    pub id: String,
    pub name: String,
    pub folder: String,
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Body>,
    pub auth: Option<Auth>,
    pub folder_vars: Vec<(String, String)>,
    pub request_vars: Vec<(String, String)>,
    pub base_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Collection {
    pub name: String,
    pub vars: Vec<(String, String)>,
    pub requests: Vec<RequestSpec>,
}

#[cfg(test)]
pub fn parse_collection(content: &str) -> Result<Collection> {
    parse_collection_at(content, PathBuf::from("."))
}

pub fn parse_collection_at(content: &str, base_dir: PathBuf) -> Result<Collection> {
    let root: Value = serde_json::from_str(content).context("collection is not valid JSON")?;
    if !root.is_object() {
        bail!("collection root must be a JSON object");
    }

    let name = root
        .get("info")
        .and_then(|info| info.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("collection")
        .to_string();
    let vars = parse_vars(root.get("variable"));
    let root_auth = parse_auth(root.get("auth")).flatten();

    let mut requests = Vec::new();
    let mut seen = HashSet::new();
    if let Some(items) = root.get("item").and_then(Value::as_array) {
        for item in items {
            flatten_item(
                item,
                &[],
                &[],
                &root_auth,
                &base_dir,
                &mut requests,
                &mut seen,
            )?;
        }
    }

    Ok(Collection {
        name,
        vars,
        requests,
    })
}

fn flatten_item(
    item: &Value,
    folder_path: &[String],
    folder_vars: &[(String, String)],
    inherited_auth: &Option<Auth>,
    base_dir: &Path,
    out: &mut Vec<RequestSpec>,
    seen: &mut HashSet<String>,
) -> Result<()> {
    let Some(obj) = item.as_object() else {
        return Ok(());
    };
    let item_name = obj
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("untitled")
        .to_string();

    let auth = match parse_auth(obj.get("auth")) {
        Some(auth) => auth,
        None => inherited_auth.clone(),
    };

    if let Some(children) = obj.get("item").and_then(Value::as_array) {
        let mut next_path = folder_path.to_vec();
        next_path.push(item_name);
        let mut next_vars = folder_vars.to_vec();
        merge_pairs(&mut next_vars, parse_vars(obj.get("variable")));
        for child in children {
            flatten_item(
                child,
                &next_path,
                &next_vars,
                &auth,
                base_dir,
                out,
                seen,
            )?;
        }
        return Ok(());
    }

    let Some(request) = obj.get("request") else {
        return Ok(());
    };
    let Some(spec) = parse_request(obj, request, folder_path, folder_vars, &auth, base_dir)? else {
        return Ok(());
    };
    let id = unique_id(spec.id, seen);
    out.push(RequestSpec { id, ..spec });
    Ok(())
}

fn parse_request(
    item: &serde_json::Map<String, Value>,
    request: &Value,
    folder_path: &[String],
    folder_vars: &[(String, String)],
    inherited_auth: &Option<Auth>,
    base_dir: &Path,
) -> Result<Option<RequestSpec>> {
    let (method, url, mut headers, body_and_ct, request_auth) = match request {
        Value::String(url) => ("GET".to_string(), url.clone(), Vec::new(), None, None),
        Value::Object(obj) => (
            obj.get("method")
                .and_then(Value::as_str)
                .unwrap_or("GET")
                .to_ascii_uppercase(),
            build_url(obj.get("url")),
            parse_headers(obj.get("header")),
            parse_body(obj.get("body"))?,
            obj.get("auth"),
        ),
        _ => return Ok(None),
    };

    if url.is_empty() {
        return Ok(None);
    }

    let (body, body_content_type) = match body_and_ct {
        Some((body, content_type)) => (Some(body), content_type),
        None => (None, None),
    };

    if let Some(content_type) = body_content_type {
        let has_content_type = headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-type"));
        if !has_content_type {
            headers.push(("Content-Type".to_string(), content_type.to_string()));
        }
    }

    let auth = match parse_auth(request_auth) {
        Some(auth) => auth,
        None => inherited_auth.clone(),
    };

    let mut request_vars = parse_vars(item.get("variable"));
    if let Value::Object(obj) = request {
        merge_pairs(&mut request_vars, parse_vars(obj.get("variable")));
    }

    let id = item
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            item.get("request")
                .and_then(|r| r.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            let mut slug = String::new();
            for part in folder_path
                .iter()
                .map(String::as_str)
                .chain(std::iter::once(item_name(item)))
            {
                if !slug.is_empty() {
                    slug.push('-');
                }
                slug.push_str(&slugify(part));
            }
            slug
        });

    Ok(Some(RequestSpec {
        id,
        name: item_name(item).to_string(),
        folder: if folder_path.is_empty() {
            "-".to_string()
        } else {
            folder_path.join(" / ")
        },
        method,
        url,
        headers,
        body,
        auth,
        folder_vars: folder_vars.to_vec(),
        request_vars,
        base_dir: base_dir.to_path_buf(),
    }))
}

fn item_name(item: &serde_json::Map<String, Value>) -> &str {
    item.get("name").and_then(Value::as_str).unwrap_or("untitled")
}

fn build_url(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return String::new();
    };

    match value {
        Value::String(raw) => raw.clone(),
        Value::Object(obj) => {
            let raw = obj.get("raw").and_then(Value::as_str);
            let query = obj.get("query").and_then(Value::as_array);
            let mut url = match raw {
                Some(raw) if query.is_some() => {
                    let cut = raw.find(['?', '#']).unwrap_or(raw.len());
                    raw[..cut].to_string()
                }
                Some(raw) => raw.to_string(),
                None => {
                    let host = host_string(obj.get("host"));
                    let path = path_string(obj.get("path"));
                    let mut url = host;
                    if !path.is_empty() {
                        if !url.is_empty() && !url.ends_with('/') {
                            url.push('/');
                        }
                        url.push_str(path.trim_start_matches('/'));
                    }
                    url
                }
            };

            if let Some(query) = query {
                let params = enabled_query(query);
                if !params.is_empty() {
                    if url.contains('?') {
                        if !url.ends_with('?') && !url.ends_with('&') {
                            url.push('&');
                        }
                    } else {
                        url.push('?');
                    }
                    url.push_str(&params.join("&"));
                }
            }
            url
        }
        _ => String::new(),
    }
}

fn host_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(host)) => host.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("."),
        _ => String::new(),
    }
}

fn path_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(path)) => path.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("/"),
        _ => String::new(),
    }
}

fn enabled_query(query: &[Value]) -> Vec<String> {
    let mut params = Vec::new();
    for param in query {
        let Some(obj) = param.as_object() else {
            continue;
        };
        if obj.get("disabled").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(key) = obj.get("key").and_then(Value::as_str) else {
            continue;
        };
        if key.is_empty() {
            continue;
        }
        let value = scalar(obj.get("value"));
        if value.is_empty() {
            params.push(key.to_string());
        } else {
            params.push(format!("{}={}", key, value));
        }
    }
    params
}

fn parse_headers(value: Option<&Value>) -> Vec<(String, String)> {
    let Some(Value::Array(headers)) = value else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for header in headers {
        let Some(obj) = header.as_object() else {
            continue;
        };
        if obj.get("disabled").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(key) = obj.get("key").and_then(Value::as_str) else {
            continue;
        };
        if key.trim().is_empty() {
            continue;
        }
        out.push((key.trim().to_string(), scalar(obj.get("value"))));
    }
    out
}

fn parse_body(value: Option<&Value>) -> Result<Option<(Body, Option<&'static str>)>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(obj) = value.as_object() else {
        bail!("request body must be a JSON object");
    };
    let Some(mode) = obj.get("mode").and_then(Value::as_str) else {
        return Ok(None);
    };

    match mode {
        "raw" => {
            let raw = obj.get("raw").and_then(Value::as_str).unwrap_or("");
            if raw.is_empty() {
                return Ok(None);
            }
            let content_type = if serde_json::from_str::<Value>(raw).is_ok() {
                "application/json"
            } else {
                "text/plain"
            };
            Ok(Some((Body::Raw(raw.to_string()), Some(content_type))))
        }
        "urlencoded" => {
            let fields = parse_urlencoded(obj.get("urlencoded"));
            Ok(Some((
                Body::UrlEncoded(fields),
                Some("application/x-www-form-urlencoded"),
            )))
        }
        "formdata" => Ok(Some((Body::FormData(parse_formdata(obj.get("formdata"))?), None))),
        "graphql" => {
            let graphql = obj.get("graphql").and_then(Value::as_object);
            let query = graphql
                .and_then(|g| g.get("query"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let variables = match graphql.and_then(|g| g.get("variables")) {
                Some(Value::String(text)) => text.clone(),
                Some(value @ Value::Object(_)) | Some(value @ Value::Array(_)) => value.to_string(),
                _ => String::new(),
            };
            Ok(Some((
                Body::GraphQL { query, variables },
                Some("application/json"),
            )))
        }
        other => bail!("unsupported Postman body mode: {}", other),
    }
}

fn parse_urlencoded(value: Option<&Value>) -> Vec<(String, String)> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        let Some(obj) = item.as_object() else {
            continue;
        };
        if obj.get("disabled").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(key) = obj.get("key").and_then(Value::as_str) else {
            continue;
        };
        if key.is_empty() {
            continue;
        }
        out.push((key.to_string(), scalar(obj.get("value"))));
    }
    out
}

fn parse_formdata(value: Option<&Value>) -> Result<Vec<FormField>> {
    let Some(Value::Array(items)) = value else {
        return Ok(Vec::new());
    };
    let mut fields = Vec::new();
    for item in items {
        let Some(obj) = item.as_object() else {
            continue;
        };
        if obj.get("disabled").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(key) = obj.get("key").and_then(Value::as_str) else {
            continue;
        };
        if key.is_empty() {
            continue;
        }
        let is_file = obj.get("type").and_then(Value::as_str) == Some("file");
        if is_file {
            let src = match obj.get("src") {
                Some(Value::String(src)) => src.clone(),
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(Value::as_str)
                    .next()
                    .unwrap_or("")
                    .to_string(),
                _ => String::new(),
            };
            if src.is_empty() {
                bail!("form-data file field '{}' has no src", key);
            }
            fields.push(FormField {
                key: key.to_string(),
                value: src,
                is_file: true,
            });
        } else {
            fields.push(FormField {
                key: key.to_string(),
                value: scalar(obj.get("value")),
                is_file: false,
            });
        }
    }
    Ok(fields)
}

fn parse_vars(value: Option<&Value>) -> Vec<(String, String)> {
    let Some(Value::Array(vars)) = value else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for var in vars {
        let Some(obj) = var.as_object() else {
            continue;
        };
        if obj.get("enabled").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        let Some(key) = obj.get("key").and_then(Value::as_str) else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        out.push((key.to_string(), scalar(obj.get("value"))));
    }
    out
}

fn parse_auth(value: Option<&Value>) -> Option<Option<Auth>> {
    let obj = value?.as_object()?;
    let auth_type = obj.get("type").and_then(Value::as_str)?;

    if auth_type == "noauth" {
        return Some(None);
    }

    let params = obj
        .get(auth_type)
        .and_then(Value::as_array)
        .map(|params| params_map(params))
        .unwrap_or_default();

    let auth = match auth_type {
        "bearer" => params
            .get("token")
            .filter(|token| !token.is_empty())
            .map(|token| Auth::Bearer(token.clone())),
        "basic" => {
            let username = params.get("username").cloned().unwrap_or_default();
            let password = params.get("password").cloned().unwrap_or_default();
            if username.is_empty() && password.is_empty() {
                None
            } else {
                Some(Auth::Basic { username, password })
            }
        }
        "apikey" => {
            let key = params.get("key").cloned().unwrap_or_default();
            let value = params.get("value").cloned().unwrap_or_default();
            if key.is_empty() {
                None
            } else {
                let location = match params.get("in").map(String::as_str) {
                    Some("query") => ApiKeyLocation::Query,
                    _ => ApiKeyLocation::Header,
                };
                Some(Auth::ApiKey {
                    key,
                    value,
                    location,
                })
            }
        }
        _ => None,
    };

    Some(auth)
}

fn params_map(params: &[Value]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for param in params {
        let Some(obj) = param.as_object() else {
            continue;
        };
        let Some(key) = obj.get("key").and_then(Value::as_str) else {
            continue;
        };
        map.insert(key.to_string(), scalar(obj.get("value")));
    }
    map
}

fn scalar(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        _ => String::new(),
    }
}

fn merge_pairs(target: &mut Vec<(String, String)>, extra: Vec<(String, String)>) {
    for (key, value) in extra {
        if let Some(existing) = target.iter_mut().find(|(k, _)| *k == key) {
            existing.1 = value;
        } else {
            target.push((key, value));
        }
    }
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(ch.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    slug
}

fn unique_id(base: String, seen: &mut HashSet<String>) -> String {
    let base = if base.is_empty() {
        "request".to_string()
    } else {
        base
    };
    let mut id = base.clone();
    let mut n = 2;
    while seen.contains(&id) {
        id = format!("{}-{}", base, n);
        n += 1;
    }
    seen.insert(id.clone());
    id
}

pub fn base_vars(
    collection: &Collection,
    env: &[(String, String)],
    overrides: &[(String, String)],
) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    for layer in [&collection.vars, env, overrides] {
        for (key, value) in layer {
            vars.insert(key.clone(), value.clone());
        }
    }
    vars
}

pub fn vars_for_request(
    collection: &Collection,
    request: &RequestSpec,
    env: &[(String, String)],
    overrides: &[(String, String)],
) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    for layer in [
        &collection.vars,
        &request.folder_vars,
        &request.request_vars,
        env,
        overrides,
    ] {
        for (key, value) in layer {
            vars.insert(key.clone(), value.clone());
        }
    }
    vars
}

pub fn resolve_vars(vars: &HashMap<String, String>) -> BTreeMap<String, String> {
    vars.iter()
        .map(|(key, value)| (key.clone(), interpolate(value, vars)))
        .collect()
}

pub fn apply_auth(
    request: &RequestSpec,
    vars: &HashMap<String, String>,
) -> (Vec<(String, String)>, String) {
    let mut headers = request.headers.clone();
    let mut url = request.url.clone();

    let Some(auth) = &request.auth else {
        return (headers, url);
    };

    let has_authorization = || {
        headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("authorization"))
    };

    match auth {
        Auth::Bearer(token) => {
            if !has_authorization() {
                headers.push((
                    "Authorization".to_string(),
                    format!("Bearer {}", interpolate(token, vars)),
                ));
            }
        }
        Auth::Basic { username, password } => {
            if !has_authorization() {
                let raw = format!(
                    "{}:{}",
                    interpolate(username, vars),
                    interpolate(password, vars)
                );
                headers.push((
                    "Authorization".to_string(),
                    format!("Basic {}", base64(raw.as_bytes())),
                ));
            }
        }
        Auth::ApiKey {
            key,
            value,
            location,
        } => {
            let key = interpolate(key, vars);
            let value = interpolate(value, vars);
            match location {
                ApiKeyLocation::Header => {
                    if !headers
                        .iter()
                        .any(|(name, _)| name.eq_ignore_ascii_case(&key))
                    {
                        headers.push((key, value));
                    }
                }
                ApiKeyLocation::Query => {
                    if url.contains('?') {
                        if !url.ends_with('?') && !url.ends_with('&') {
                            url.push('&');
                        }
                    } else {
                        url.push('?');
                    }
                    url.push_str(&key);
                    url.push('=');
                    url.push_str(&value);
                }
            }
        }
    }

    (headers, url)
}

fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

pub fn load_environment(path: &Path) -> Result<Vec<(String, String)>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read environment file: {}", path.display()))?;
    if let Ok(root) = serde_json::from_str::<Value>(&content) {
        if let Some(values) = root.get("values") {
            return Ok(parse_vars(Some(values)));
        }
    }

    let mut vars = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            if !key.is_empty() {
                vars.push((key.to_string(), value.trim().to_string()));
            }
        }
    }
    Ok(vars)
}

pub fn run_cli(args: &[String]) -> Result<()> {
    let Some(command) = args.first() else {
        bail!("{}", usage());
    };

    match command.as_str() {
        "list" => {
            let file = args.get(1).context("collection list requires a collection.json")?;
            let collection = load_collection(Path::new(file))?;
            if collection.requests.is_empty() {
                println!("(no requests)");
                return Ok(());
            }
            for request in &collection.requests {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    request.id,
                    request.name,
                    request.folder,
                    request.method,
                    request.url
                );
            }
            Ok(())
        }
        "env" => {
            let file = args.get(1).context("collection env requires a collection.json")?;
            let collection = load_collection(Path::new(file))?;
            let mut env_file: Option<String> = None;
            let mut overrides = Vec::new();

            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--env" => {
                        i += 1;
                        env_file = Some(args.get(i).context("--env requires a file")?.clone());
                    }
                    "--var" => {
                        i += 1;
                        overrides.push(parse_override(
                            args.get(i).context("--var requires key=value")?,
                        )?);
                    }
                    other => bail!("unknown collection env option: {}\n\n{}", other, usage()),
                }
                i += 1;
            }

            let env_path = env_file.context("collection env requires --env <environment.json>")?;
            let env = load_environment(Path::new(&env_path))?;
            let vars = base_vars(&collection, &env, &overrides);
            for (key, value) in resolve_vars(&vars) {
                println!("{}={}", key, value);
            }
            Ok(())
        }
        "run" => run_collection(args),
        other => bail!("unknown collection command: {}\n\n{}", other, usage()),
    }
}

fn run_collection(args: &[String]) -> Result<()> {
    let file = args.get(1).context("collection run requires a collection.json")?;
    let mut request_name: Option<String> = None;
    let mut all = false;
    let mut as_json = false;
    let mut fail_fast = false;
    let mut follow = false;
    let mut env_file: Option<String> = None;
    let mut overrides = Vec::new();
    let mut history: Option<String> = None;

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--request" => {
                i += 1;
                request_name = Some(
                    args.get(i)
                        .context("--request requires a name or id")?
                        .clone(),
                );
            }
            "--all" => all = true,
            "--json" => as_json = true,
            "--fail-fast" => fail_fast = true,
            "--follow" => follow = true,
            "--history" => {
                i += 1;
                history = Some(args.get(i).context("--history requires a file")?.clone());
            }
            "--env" => {
                i += 1;
                env_file = Some(args.get(i).context("--env requires a file")?.clone());
            }
            "--var" => {
                i += 1;
                overrides.push(parse_override(
                    args.get(i).context("--var requires key=value")?,
                )?);
            }
            other => bail!("unknown collection run option: {}\n\n{}", other, usage()),
        }
        i += 1;
    }

    if !all && request_name.is_none() {
        bail!(
            "collection run requires --request <name|id> or --all\n\n{}",
            usage()
        );
    }
    if all && request_name.is_some() {
        bail!("use either --request or --all, not both");
    }

    let collection = load_collection(Path::new(file))?;
    let env = match &env_file {
        Some(path) => load_environment(Path::new(path))?,
        None => Vec::new(),
    };

    if all {
        return run_all(
            &collection,
            &env,
            &overrides,
            as_json,
            fail_fast,
            follow,
            history.as_deref(),
        );
    }

    let wanted = request_name.unwrap();
    let request = find_request(&collection, &wanted)?;
    let vars = vars_for_request(&collection, request, &env, &overrides);
    execute_request(
        &collection,
        request,
        &vars,
        follow,
        as_json,
        history.as_deref(),
    )?;
    Ok(())
}

fn run_all(
    collection: &Collection,
    env: &[(String, String)],
    overrides: &[(String, String)],
    as_json: bool,
    fail_fast: bool,
    follow: bool,
    history: Option<&str>,
) -> Result<()> {
    let mut results: Vec<Outcome> = Vec::new();
    let mut stopped_early = false;

    for request in &collection.requests {
        let vars = vars_for_request(collection, request, env, overrides);
        let url = resolved_url(request, &vars);
        if let Some(var) = unresolved_var(&url) {
            let outcome = Outcome::failed(
                request,
                url,
                format!("unresolved variable {} in URL", var),
            );
            let failed = outcome.is_failed();
            results.push(outcome);
            if fail_fast && failed {
                stopped_early = true;
                break;
            }
            continue;
        }

        let outcome = match execute_prepared(request, &vars, follow) {
            Ok(result) => Outcome::ok(request, url, result),
            Err(err) => Outcome::failed(request, url, err.to_string()),
        };
        if let (Some(path), Some(result)) = (history, outcome.result.as_ref()) {
            append_history(
                Path::new(path),
                &collection.name,
                &outcome.id,
                &outcome.name,
                &outcome.method,
                &outcome.url,
                result,
            )?;
        }
        let failed = outcome.is_failed();
        results.push(outcome);
        if fail_fast && failed {
            stopped_early = true;
            break;
        }
    }

    if as_json {
        print_all_json(collection, &results, stopped_early)?;
    } else {
        print_all_human(&results);
    }

    if stopped_early {
        bail!("fail-fast: stopped after the first failed request");
    }
    Ok(())
}

struct Outcome {
    id: String,
    name: String,
    folder: String,
    method: String,
    url: String,
    result: Option<ExecResult>,
    error: Option<String>,
}

impl Outcome {
    fn ok(request: &RequestSpec, url: String, result: ExecResult) -> Self {
        Self {
            id: request.id.clone(),
            name: request.name.clone(),
            folder: request.folder.clone(),
            method: request.method.clone(),
            url,
            result: Some(result),
            error: None,
        }
    }

    fn failed(request: &RequestSpec, url: String, error: String) -> Self {
        Self {
            id: request.id.clone(),
            name: request.name.clone(),
            folder: request.folder.clone(),
            method: request.method.clone(),
            url,
            result: None,
            error: Some(error),
        }
    }

    fn is_failed(&self) -> bool {
        match &self.result {
            Some(result) => !(200..400).contains(&result.status),
            None => true,
        }
    }

    fn status(&self) -> u16 {
        self.result.as_ref().map(|r| r.status).unwrap_or(0)
    }

    fn time_ms(&self) -> u128 {
        self.result.as_ref().map(|r| r.time_ms).unwrap_or(0)
    }

    fn size(&self) -> usize {
        self.result.as_ref().map(|r| r.size).unwrap_or(0)
    }

}

fn execute_request(
    collection: &Collection,
    request: &RequestSpec,
    vars: &HashMap<String, String>,
    follow: bool,
    as_json: bool,
    history: Option<&str>,
) -> Result<()> {
    let result = execute_prepared(request, vars, follow)?;
    let url = resolved_url(request, vars);
    if let Some(path) = history {
        append_history(
            Path::new(path),
            &collection.name,
            &request.id,
            &request.name,
            &request.method,
            &url,
            &result,
        )?;
    }
    if as_json {
        let value = json_result(
            &request.id,
            &request.name,
            &request.folder,
            &request.method,
            &url,
            &result,
            None,
        );
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        print_human(&result);
    }
    Ok(())
}

fn resolved_url(request: &RequestSpec, vars: &HashMap<String, String>) -> String {
    let (_, url) = apply_auth(request, vars);
    interpolate(&url, vars)
}

fn execute_prepared(
    request: &RequestSpec,
    vars: &HashMap<String, String>,
    follow: bool,
) -> Result<ExecResult> {
    let (headers, url) = apply_auth(request, vars);
    let prepared = Request {
        name: request.name.clone(),
        method: request.method.clone(),
        url,
        headers,
        body: request.body.clone(),
        body_dir: request.base_dir.clone(),
    };
    execute(&prepared, vars, follow)
}

fn find_request<'a>(collection: &'a Collection, wanted: &str) -> Result<&'a RequestSpec> {
    if let Some(request) = collection.requests.iter().find(|r| r.id == wanted) {
        return Ok(request);
    }
    let matches: Vec<&RequestSpec> = collection
        .requests
        .iter()
        .filter(|r| r.name == wanted)
        .collect();
    match matches.as_slice() {
        [request] => Ok(request),
        [] => {
            let available = collection
                .requests
                .iter()
                .map(|r| format!("{} ({})", r.name, r.id))
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "request '{}' not found (available: {})",
                wanted,
                available
            )
        }
        _ => {
            let ids = matches
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "request name '{}' is ambiguous; use an id: {}",
                wanted,
                ids
            )
        }
    }
}

fn print_human(result: &ExecResult) {
    println!(
        "HTTP {} ({} ms, {} B)",
        result.status, result.time_ms, result.size
    );
    for (name, value) in &result.headers {
        println!("{}: {}", name, value);
    }
    println!();
    print!("{}", result.body);
    if !result.body.ends_with('\n') {
        println!();
    }
}

fn print_all_human(results: &[Outcome]) {
    for (index, outcome) in results.iter().enumerate() {
        if index > 0 {
            println!();
        }
        println!("=== {} ===", outcome.name);
        match &outcome.result {
            Some(result) => print_human(result),
            None => {
                if let Some(error) = &outcome.error {
                    println!("ERROR: {}", error);
                }
            }
        }
    }

    let total = results.len();
    let failed = results.iter().filter(|r| r.is_failed()).count();
    let ok = total - failed;
    let time: u128 = results.iter().map(Outcome::time_ms).sum();

    println!();
    println!(
        "Summary: {} request{}, {} ok, {} failed, {} ms total",
        total,
        if total == 1 { "" } else { "s" },
        ok,
        failed,
        time
    );
    for outcome in results {
        println!(
            "  {:<22} {:>3}  {:>6} ms  {:>7} B  {}",
            truncate(&outcome.name, 22),
            outcome.status(),
            outcome.time_ms(),
            outcome.size(),
            if outcome.is_failed() { "fail" } else { "ok" }
        );
    }
}

fn print_all_json(
    collection: &Collection,
    results: &[Outcome],
    stopped_early: bool,
) -> Result<()> {
    let total = results.len();
    let failed = results.iter().filter(|r| r.is_failed()).count();
    let time: u128 = results.iter().map(Outcome::time_ms).sum();
    let entries = results
        .iter()
        .map(|outcome| {
            match &outcome.result {
                Some(result) => json_result(
                    &outcome.id,
                    &outcome.name,
                    &outcome.folder,
                    &outcome.method,
                    &outcome.url,
                    result,
                    outcome.error.as_deref(),
                ),
                None => json_result(
                    &outcome.id,
                    &outcome.name,
                    &outcome.folder,
                    &outcome.method,
                    &outcome.url,
                    &ExecResult {
                        status: 0,
                        headers: Vec::new(),
                        body: String::new(),
                        time_ms: 0,
                        size: 0,
                    },
                    outcome.error.as_deref(),
                ),
            }
        })
        .collect::<Vec<_>>();

    let value = serde_json::json!({
        "collection": collection.name,
        "results": entries,
        "summary": {
            "total": total,
            "ok": total - failed,
            "failed": failed,
            "time_ms": time,
            "stopped_early": stopped_early,
        }
    });
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn json_result(
    id: &str,
    name: &str,
    folder: &str,
    method: &str,
    url: &str,
    result: &ExecResult,
    error: Option<&str>,
) -> Value {
    let mut headers = serde_json::Map::new();
    for (header, value) in &result.headers {
        if let Some(Value::String(existing)) = headers.get(header) {
            headers.insert(
                header.clone(),
                Value::String(format!("{}, {}", existing, value)),
            );
        } else {
            headers.insert(header.clone(), Value::String(value.clone()));
        }
    }
    let mut value = serde_json::json!({
        "id": id,
        "name": name,
        "folder": folder,
        "method": method,
        "url": url,
        "status": result.status,
        "time_ms": result.time_ms,
        "size": result.size,
        "headers": Value::Object(headers),
        "body": result.body,
    });
    if let Some(error) = error {
        value["error"] = Value::String(error.to_string());
    }
    value
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out = value.chars().take(max.saturating_sub(1)).collect::<String>();
    out.push('…');
    out
}

fn parse_override(raw: &str) -> Result<(String, String)> {
    let (key, value) = raw
        .split_once('=')
        .with_context(|| format!("invalid --var (expected key=value): {}", raw))?;
    Ok((key.trim().to_string(), value.trim().to_string()))
}

fn load_collection(path: &Path) -> Result<Collection> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read collection: {}", path.display()))?;
    let base_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    parse_collection_at(&content, base_dir)
}

fn usage() -> &'static str {
    "usage:\n  php-http collection list <collection.json>\n  php-http collection env <collection.json> --env <environment.json> [--var key=value]...\n  php-http collection run <collection.json> --request <name|id> [--env <environment.json>] [--var key=value]... [--json] [--follow] [--history <file>]\n  php-http collection run <collection.json> --all [--env <environment.json>] [--var key=value]... [--json] [--fail-fast] [--follow] [--history <file>]"
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../../fixtures/postman/demo.postman_collection.json");

    #[test]
    fn parses_nested_folders_and_requests() {
        let collection = parse_collection(FIXTURE).expect("fixture parses");
        assert_eq!(collection.name, "Demo API");
        assert_eq!(collection.requests.len(), 6);

        let get = collection
            .requests
            .iter()
            .find(|r| r.id == "req-get-items")
            .expect("GET items");
        assert_eq!(get.name, "GET items");
        assert_eq!(get.folder, "Items");
        assert_eq!(get.method, "GET");
        assert_eq!(get.url, "{{host}}/items?limit={{itemLimit}}");
        assert!(get
            .headers
            .iter()
            .any(|(k, v)| k == "Accept" && v == "application/json"));
        assert!(!get.headers.iter().any(|(k, _)| k == "X-Debug"));

        let post = collection
            .requests
            .iter()
            .find(|r| r.id == "req-post-items")
            .expect("POST items");
        assert_eq!(post.method, "POST");
        assert_eq!(
            post.body,
            Some(Body::Raw("{\"name\":\"demo\"}".to_string()))
        );
        assert_eq!(post.auth, Some(Auth::Bearer("{{token}}".to_string())));
        assert!(post
            .headers
            .iter()
            .any(|(k, v)| k == "Content-Type" && v == "application/json"));

        let missing = collection
            .requests
            .iter()
            .find(|r| r.id == "req-missing")
            .expect("GET missing");
        assert_eq!(missing.folder, "Missing");
        assert_eq!(missing.url, "{{host}}/missing");
    }

    #[test]
    fn variable_precedence_is_collection_then_environment_then_override() {
        let collection = parse_collection(FIXTURE).expect("fixture parses");
        let env = vec![
            ("host".to_string(), "http://env".to_string()),
            ("token".to_string(), "secret".to_string()),
        ];
        let overrides = vec![("token".to_string(), "override".to_string())];

        let vars = base_vars(&collection, &env, &overrides);
        assert_eq!(vars.get("host").map(String::as_str), Some("http://env"));
        assert_eq!(vars.get("token").map(String::as_str), Some("override"));
        assert_eq!(vars.get("apiVersion").map(String::as_str), Some("v1"));
    }

    #[test]
    fn request_vars_add_folder_scope_below_environment() {
        let collection = parse_collection(FIXTURE).expect("fixture parses");
        let get = collection
            .requests
            .iter()
            .find(|r| r.id == "req-get-items")
            .expect("GET items");
        let env = vec![("host".to_string(), "http://env".to_string())];

        let vars = vars_for_request(&collection, get, &env, &[]);
        assert_eq!(vars.get("itemLimit").map(String::as_str), Some("2"));
        assert_eq!(
            interpolate(&get.url, &vars),
            "http://env/items?limit=2"
        );
    }

    #[test]
    fn resolves_nested_variable_values() {
        let mut vars = HashMap::new();
        vars.insert("host".to_string(), "http://127.0.0.1:8765".to_string());
        vars.insert("base".to_string(), "{{host}}/api".to_string());
        vars.insert("token".to_string(), "secret".to_string());

        let resolved = resolve_vars(&vars);
        assert_eq!(resolved.get("base").map(String::as_str), Some("http://127.0.0.1:8765/api"));
        assert_eq!(
            interpolate("{{base}}/items?token={{token}}", &vars),
            "http://127.0.0.1:8765/api/items?token=secret"
        );
    }

    #[test]
    fn parses_basic_and_apikey_auth() {
        let basic = parse_collection(
            r#"{
                "info": {"name": "auth"},
                "auth": {"type": "basic", "basic": [
                    {"key": "username", "value": "user"},
                    {"key": "password", "value": "pass"}
                ]},
                "item": [{
                    "name": "Basic",
                    "request": {"method": "GET", "url": "http://example.test"}
                }, {
                    "name": "Api key",
                    "request": {
                        "method": "GET",
                        "url": "http://example.test",
                        "auth": {"type": "apikey", "apikey": [
                            {"key": "key", "value": "X-API-Key"},
                            {"key": "value", "value": "{{token}}"},
                            {"key": "in", "value": "query"}
                        ]}
                    }
                }]
            }"#,
        )
        .expect("auth collection parses");

        assert_eq!(
            basic.requests[0].auth,
            Some(Auth::Basic {
                username: "user".to_string(),
                password: "pass".to_string()
            })
        );
        let vars = HashMap::from([("token".to_string(), "abc".to_string())]);
        let (headers, url) = apply_auth(&basic.requests[1], &vars);
        assert!(headers.is_empty());
        assert_eq!(url, "http://example.test?X-API-Key=abc");
    }

    #[test]
    fn basic_auth_is_base64_encoded() {
        let auth = Auth::Basic {
            username: "user".to_string(),
            password: "pass".to_string(),
        };
        let request = RequestSpec {
            id: "id".to_string(),
            name: "name".to_string(),
            folder: "-".to_string(),
            method: "GET".to_string(),
            url: "http://example.test".to_string(),
            headers: Vec::new(),
            body: None,
            auth: Some(auth),
            folder_vars: Vec::new(),
            request_vars: Vec::new(),
            base_dir: PathBuf::from("."),
        };
        let (headers, _) = apply_auth(&request, &HashMap::new());
        assert_eq!(headers, vec![("Authorization".to_string(), "Basic dXNlcjpwYXNz".to_string())]);
    }

    #[test]
    fn parses_raw_urlencoded_formdata_and_graphql_bodies() {
        let collection = parse_collection(FIXTURE).expect("fixture parses");

        let urlencoded = collection
            .requests
            .iter()
            .find(|r| r.id == "req-urlencoded")
            .expect("urlencoded request");
        assert_eq!(
            urlencoded.body,
            Some(Body::UrlEncoded(vec![
                ("full name".to_string(), "Ada Lovelace".to_string()),
                ("note".to_string(), "a+b & c=d".to_string()),
            ]))
        );
        assert!(urlencoded.headers.iter().any(|(k, v)| {
            k == "Content-Type" && v == "application/x-www-form-urlencoded"
        }));
        assert_eq!(
            urlencoded.auth,
            Some(Auth::Bearer("{{token}}".to_string()))
        );

        let formdata = collection
            .requests
            .iter()
            .find(|r| r.id == "req-formdata")
            .expect("formdata request");
        assert_eq!(
            formdata.body,
            Some(Body::FormData(vec![
                FormField {
                    key: "title".to_string(),
                    value: "hello".to_string(),
                    is_file: false,
                },
                FormField {
                    key: "full name".to_string(),
                    value: "Ada Lovelace".to_string(),
                    is_file: false,
                },
                FormField {
                    key: "upload".to_string(),
                    value: "upload.txt".to_string(),
                    is_file: true,
                },
            ]))
        );
        assert!(!formdata
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("content-type")));

        let graphql = collection
            .requests
            .iter()
            .find(|r| r.id == "req-graphql")
            .expect("graphql request");
        assert_eq!(
            graphql.body,
            Some(Body::GraphQL {
                query: "query($id: Int!) { item(id: $id) { name } }".to_string(),
                variables: "{\"id\": 7}".to_string(),
            })
        );
        assert!(graphql
            .headers
            .iter()
            .any(|(k, v)| k == "Content-Type" && v == "application/json"));
    }

    #[test]
    fn rejects_unknown_body_mode() {
        let err = parse_collection(
            r#"{
                "info": {"name": "bodies"},
                "item": [{
                    "name": "binary",
                    "request": {
                        "method": "POST",
                        "url": "http://example.test",
                        "body": {"mode": "binary", "file": {"src": "x"}}
                    }
                }]
            }"#,
        )
        .expect_err("unknown mode fails");
        assert!(err.to_string().contains("unsupported Postman body mode: binary"));
    }

    #[test]
    fn parses_environment_json_and_key_value_fallback() {
        let dir = std::env::temp_dir().join(format!("php-http-env-test-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let json_path = dir.join("env.json");
        fs::write(
            &json_path,
            r#"{"values": [
                {"key": "host", "value": "http://127.0.0.1:8765", "enabled": true},
                {"key": "token", "value": "secret", "enabled": true},
                {"key": "disabled", "value": "nope", "enabled": false}
            ]}"#,
        )
        .expect("write env");
        let vars = load_environment(&json_path).expect("env loads");
        assert_eq!(vars.len(), 2);
        assert!(vars.contains(&("token".to_string(), "secret".to_string())));
        assert!(!vars.iter().any(|(k, _)| k == "disabled"));

        let kv_path = dir.join("env.txt");
        fs::write(&kv_path, "# comment\nhost=http://localhost\n").expect("write kv");
        let vars = load_environment(&kv_path).expect("kv loads");
        assert_eq!(vars, vec![("host".to_string(), "http://localhost".to_string())]);

        let _ = fs::remove_dir_all(&dir);
    }
}
