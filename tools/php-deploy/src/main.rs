use std::collections::HashMap;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Clone)]
struct Config {
    #[serde(default)]
    default: Option<String>,
    servers: Vec<Server>,
}

#[derive(Debug, Serialize)]
struct LsEntry {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    size: Option<u64>,
    modified: Option<u64>,
}

#[derive(Debug, Serialize)]
struct LsResponse {
    server: String,
    path: String,
    entries: Vec<LsEntry>,
}

#[derive(Debug, Serialize)]
struct DownloadResponse {
    server: String,
    path: String,
    to: String,
    bytes: u64,
}

#[derive(Debug, Serialize)]
struct DeleteResponse {
    server: String,
    path: String,
    deleted: bool,
}

#[derive(Debug, Default)]
struct Args {
    config_path: Option<PathBuf>,
    server_name: Option<String>,
    dry_run: bool,
    interval: u64,
    file: Option<PathBuf>,
    command: Option<String>,
    remote_path: Option<String>,
    to: Option<PathBuf>,
    allow_delete: bool,
    recursive: bool,
    json: bool,
    help: bool,
    version: bool,
}

impl Args {
    fn parse<I>(arguments: I) -> Result<Self>
    where
        I: IntoIterator<Item = String>,
    {
        let mut parsed = Args {
            interval: 2,
            ..Args::default()
        };
        let mut args = arguments.into_iter();
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--config" => {
                    parsed.config_path = Some(PathBuf::from(args.next().context("--config path")?))
                }
                "--server" => parsed.server_name = Some(args.next().context("--server name")?),
                "--file" => parsed.file = Some(PathBuf::from(args.next().context("--file path")?)),
                "--path" => parsed.remote_path = Some(args.next().context("--path REMOTE_PATH")?),
                "--to" => parsed.to = Some(PathBuf::from(args.next().context("--to LOCAL_PATH")?)),
                "--json" => parsed.json = true,
                "--allow-delete" => parsed.allow_delete = true,
                "--recursive" => parsed.recursive = true,
                "--interval" => {
                    parsed.interval = args
                        .next()
                        .context("--interval seconds")?
                        .parse()
                        .context("invalid --interval")?
                }
                "--dry-run" => parsed.dry_run = true,
                "-h" | "--help" => parsed.help = true,
                "--version" => parsed.version = true,
                other if !other.starts_with('-') => {
                    if parsed.command.is_some() {
                        bail!("unexpected argument: {other}");
                    }
                    parsed.command = Some(other.to_string());
                }
                other => bail!("unknown argument: {other}"),
            }
        }
        Ok(parsed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SshAuth {
    #[default]
    Agent,
    Password,
    Key,
}

impl SshAuth {
    fn parse(value: &str) -> Option<SshAuth> {
        Some(match value {
            "agent" => SshAuth::Agent,
            "password" => SshAuth::Password,
            "key" => SshAuth::Key,
            _ => return None,
        })
    }

    fn as_str(self) -> &'static str {
        match self {
            SshAuth::Agent => "agent",
            SshAuth::Password => "password",
            SshAuth::Key => "key",
        }
    }
}

/// Custom deserialization so an invalid `ssh_auth` value fails at config load
/// instead of silently falling back to the agent default. The `Option` keeps
/// "explicitly agent" distinguishable from "not specified" (inference).
fn deserialize_ssh_auth<'de, D>(deserializer: D) -> Result<Option<SshAuth>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = match Option::<String>::deserialize(deserializer)? {
        Some(value) => value,
        None => return Ok(None),
    };
    SshAuth::parse(&value).map(Some).ok_or_else(|| {
        serde::de::Error::custom(format!(
            "invalid ssh_auth `{value}` (expected agent, password, or key)"
        ))
    })
}

/// Serializes the lowercase form so a serialized server parses back unchanged.
fn serialize_ssh_auth<S>(value: &Option<SshAuth>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value {
        Some(auth) => serializer.serialize_str(auth.as_str()),
        None => serializer.serialize_none(),
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Server {
    name: String,
    protocol: String,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    local_root: Option<String>,
    remote_root: String,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    upload_on_save: bool,
    #[serde(default)]
    ssh_key_path: Option<String>,
    #[serde(default)]
    ssh_password: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_ssh_auth",
        serialize_with = "serialize_ssh_auth"
    )]
    ssh_auth: Option<SshAuth>,
    // Advanced connection settings. Canonical on every connection; only
    // meaningful for ftp/ftps today. Stored-but-inert fields exist so a
    // config written by the panel round-trips through the CLI unchanged.
    //
    // - show_hidden: no-op — curl LIST already shows dotfiles on most
    //   servers, and curl has no flag to force them where a server hides them.
    // - use_mlsd / list_compat: no-op — the CLI never issues MLSD.
    // - max_connections / keep_alive_enabled / keep_alive_seconds / encoding:
    //   stored only — the CLI makes single-shot connections, never a pooled
    //   or persistent session.
    #[serde(default = "default_true")]
    passive_mode: bool,
    #[serde(default)]
    show_hidden: bool,
    #[serde(default)]
    use_mlsd: bool,
    #[serde(default)]
    list_compat: bool,
    #[serde(default = "default_max_connections")]
    max_connections: u32,
    #[serde(default = "default_true")]
    keep_alive_enabled: bool,
    #[serde(default = "default_keep_alive_seconds")]
    keep_alive_seconds: u32,
    #[serde(default = "default_encoding")]
    encoding: String,
}

fn default_true() -> bool {
    true
}

fn default_max_connections() -> u32 {
    5
}

fn default_keep_alive_seconds() -> u32 {
    300
}

fn default_encoding() -> String {
    "UTF-8".to_string()
}

impl Server {
    /// The effective ssh auth mode. An explicit `ssh_auth` wins; without one
    /// the mode is inferred from the provided credentials so that setting
    /// `ssh_password` or `ssh_key_path` alone is enough.
    fn resolved_auth(&self) -> SshAuth {
        if let Some(auth) = self.ssh_auth {
            return auth;
        }
        if self.ssh_password.is_some() {
            SshAuth::Password
        } else if self.ssh_key_path.is_some() {
            SshAuth::Key
        } else {
            SshAuth::Agent
        }
    }

    /// The password secret for error scrubbing; `None` outside password mode.
    fn ssh_secret(&self) -> Option<&str> {
        match self.resolved_auth() {
            SshAuth::Password => self.ssh_password.as_deref().filter(|p| !p.is_empty()),
            _ => None,
        }
    }
}

/// Removes a secret from user-visible error text. An empty secret is a no-op.
fn scrub(text: &str, secret: Option<&str>) -> String {
    match secret {
        Some(secret) if !secret.is_empty() => text.replace(secret, "***"),
        _ => text.to_string(),
    }
}

fn find_executable(bin: &str, search_path: Option<&OsStr>) -> Option<PathBuf> {
    let path = match search_path {
        Some(path) => path.to_os_string(),
        None => env::var_os("PATH")?,
    };
    for directory in env::split_paths(&path) {
        if directory.as_os_str().is_empty() {
            continue;
        }
        let candidate = directory.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn sshpass_available() -> bool {
    find_executable("sshpass", None).is_some()
}

/// Configuration warnings for the sftp auth mode, shown by `check`.
fn ssh_auth_warnings_with(server: &Server, search_path: Option<&OsStr>) -> Vec<String> {
    let mut warnings = Vec::new();
    if server.resolved_auth() == SshAuth::Password && find_executable("sshpass", search_path).is_none() {
        warnings.push("sshpass not installed — key or agent authentication required".to_string());
    }
    warnings
}

/// Builds the `ssh`/`sftp` invocation prefix for the server's auth mode.
///
/// - agent: `-o BatchMode=yes`
/// - key: `-i <key_path> -o IdentitiesOnly=yes -o BatchMode=yes`
/// - password: `sshpass -p <password> <program>` plus
///   `-o PreferredAuthentications=password -o NumberOfPasswordPrompts=1`
///   (no BatchMode — it would suppress the prompt sshpass feeds).
fn ssh_base_command_with(
    server: &Server,
    program: &str,
    search_path: Option<&OsStr>,
) -> Result<Command> {
    match server.resolved_auth() {
        SshAuth::Password => {
            let password = server
                .ssh_password
                .as_deref()
                .filter(|password| !password.is_empty())
                .context("sftp password authentication requires a non-empty `ssh_password`")?;
            if !sshpass_available_with(search_path) {
                bail!("sshpass not installed — use key or agent authentication");
            }
            let mut command = Command::new("sshpass");
            command.arg("-p").arg(password).arg(program);
            command.args([
                "-o",
                "PreferredAuthentications=password",
                "-o",
                "NumberOfPasswordPrompts=1",
            ]);
            Ok(command)
        }
        SshAuth::Key => {
            let key = server
                .ssh_key_path
                .as_deref()
                .filter(|key| !key.is_empty())
                .context("sftp key authentication requires a non-empty `ssh_key_path`")?;
            let mut command = Command::new(program);
            command.arg("-i").arg(key);
            command.args(["-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes"]);
            Ok(command)
        }
        SshAuth::Agent => {
            let mut command = Command::new(program);
            command.args(["-o", "BatchMode=yes"]);
            Ok(command)
        }
    }
}

fn sshpass_available_with(search_path: Option<&OsStr>) -> bool {
    find_executable("sshpass", search_path).is_some()
}

fn ssh_base_command(server: &Server, program: &str) -> Result<Command> {
    ssh_base_command_with(server, program, None)
}

/// Shared `curl` invocation prefix for the ftp/ftps transports (ls, upload,
/// download, and the `check` connectivity probe).
///
/// PASSIVE mode (the default) is a no-op because curl is passive by default;
/// ACTIVE mode (`passive_mode == false`) appends `-P -` so curl lets the
/// server pick the data connection port instead of an ephemeral local one.
fn curl_base_command(server: &Server) -> Result<Command> {
    match server.protocol.as_str() {
        "ftp" | "ftps" => {}
        other => bail!("curl transport requires ftp or ftps, got {other}"),
    }
    let mut command = Command::new("curl");
    if !server.passive_mode {
        command.args(["-P", "-"]);
    }
    Ok(command)
}

/// ftp/ftps URL for a path under the server root; an empty path is the root.
/// The port is included only when configured, so curl applies its own scheme
/// defaults (ftp 21, ftps 990).
fn ftp_url(server: &Server, path: &str) -> Result<String> {
    let host = server
        .host
        .as_deref()
        .filter(|host| !host.is_empty())
        .with_context(|| format!("{} requires host", server.protocol))?;
    let port = match server.port {
        Some(port) => format!(":{port}"),
        None => String::new(),
    };
    let relative = path.trim_start_matches('/');
    if relative.is_empty() {
        Ok(format!("{}://{}{port}/", server.protocol, host))
    } else {
        Ok(format!("{}://{}{port}/{relative}", server.protocol, host))
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(2);
    }
}

fn run() -> Result<()> {
    let args = Args::parse(env::args().skip(1))?;
    if args.help {
        print_help();
        return Ok(());
    }
    if args.version {
        println!("php-deploy {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let command = args.command.clone().context(
        "missing command: check | status | sync | upload | watch | ls | download | delete",
    )?;
    let (config, project_root) = load_config(args.config_path.clone())?;
    let server = select_server(&config, args.server_name.clone())?;

    match command.as_str() {
        "status" => print_status(&config, server, &project_root),
        "check" => check_server(server, &project_root, args.dry_run)?,
        "sync" => sync_server(server, &project_root, args.dry_run)?,
        "upload" => {
            let file = args.file.clone().context("upload requires --file")?;
            upload_server(server, &project_root, &file, args.dry_run)?
        }
        "watch" => watch_server(server, &project_root, args.interval)?,
        "ls" => list_server(server, args.remote_path.clone(), args.json)?,
        "download" => {
            let path = args
                .remote_path
                .clone()
                .context("download requires --path")?;
            let to = args.to.clone().context("download requires --to")?;
            download_server(server, &path, &to, args.json)?
        }
        "delete" => {
            let path = args
                .remote_path
                .clone()
                .context("delete requires --path")?;
            delete_server(server, &path, args.recursive, args.allow_delete, args.json)?
        }
        other => bail!("unknown command: {other}"),
    }

    Ok(())
}

fn print_help() {
    println!(
        "php-deploy [--config PATH] [--server NAME] [--dry-run] [--version] <command>\n\n\
         commands:\n\
           status            show configured servers and resolved paths\n\
           check             validate config and test connectivity\n\
           sync              upload all non-excluded files\n\
           upload --file P   upload one file\n\
           watch [--interval N]  poll local files and upload changes\n\
           ls [--path P] [--json]  list a remote directory\n\
           download --path P --to LOCAL [--json]  fetch a remote file\n\
           delete --path P [--recursive] --allow-delete [--json]  remove a remote entry\n\n\
         safety: --path must be relative and stay under remote_root ('..' rejected)\n\
          config: .zed/deploy.json (local_root relative to project root)\n\
          protocols: local, sftp (ssh auth: agent [default], key via\n\
          ssh_key_path, password via ssh_password + the sshpass binary)"
    );
}

fn load_config(explicit: Option<PathBuf>) -> Result<(Config, PathBuf)> {
    let path = match explicit {
        Some(path) => path,
        None => {
            let current = env::current_dir()?;
            let candidate = current.join(".zed/deploy.json");
            if candidate.exists() {
                candidate
            } else {
                bail!("no .zed/deploy.json found (or pass --config)");
            }
        }
    };

    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let config: Config = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if config.servers.is_empty() {
        bail!("no servers configured in {}", path.display());
    }

    let parent = path.parent().unwrap_or(Path::new("."));
    let project_root = if parent.file_name().and_then(|name| name.to_str()) == Some(".zed") {
        parent.parent().unwrap_or(parent).to_path_buf()
    } else {
        parent.to_path_buf()
    };

    Ok((config, project_root))
}

fn select_server(config: &Config, requested: Option<String>) -> Result<&Server> {
    let name = requested.or_else(|| config.default.clone());
    match name {
        Some(name) => config
            .servers
            .iter()
            .find(|server| server.name == name)
            .with_context(|| format!("server not found: {name}")),
        None => config.servers.first().context("no servers configured"),
    }
}

fn print_status(config: &Config, server: &Server, project_root: &Path) {
    println!("config servers: {}", config.servers.len());
    for candidate in &config.servers {
        let marker = if candidate.name == server.name { "*" } else { " " };
        println!(
            "{marker} {:<12} {:<6} {}{} root={} exclude={} auto-upload={}",
            candidate.name,
            candidate.protocol,
            candidate
                .user
                .as_deref()
                .map(|user| format!("{user}@"))
                .unwrap_or_default(),
            candidate.host.as_deref().unwrap_or("-"),
            candidate.remote_root,
            candidate.exclude.len(),
            candidate.upload_on_save,
        );
    }
    println!(
        "local root: {}",
        local_root(server, project_root).display()
    );
}

fn local_root(server: &Server, project_root: &Path) -> PathBuf {
    let root = server.local_root.as_deref().unwrap_or(".");
    project_root.join(root)
}

fn check_server(server: &Server, project_root: &Path, dry_run: bool) -> Result<()> {
    let root = local_root(server, project_root);
    if !root.exists() {
        bail!("local root does not exist: {}", root.display());
    }

    match server.protocol.as_str() {
        "local" => {
            let destination = PathBuf::from(&server.remote_root);
            println!(
                "local transport: {} -> {} ({})",
                root.display(),
                destination.display(),
                if destination.exists() { "exists" } else { "missing" }
            );
        }
        "sftp" => {
            for warning in ssh_auth_warnings_with(server, None) {
                println!("warning: {warning}");
            }
            let command = "true";
            if dry_run {
                println!("ssh check: {command}");
            } else {
                run_ssh(server, command)?;
                println!("sftp connectivity ok");
            }
        }
        "ftp" | "ftps" => {
            let url = ftp_url(server, "")?;
            if dry_run {
                println!("curl check: {url}");
            } else {
                let mut command = curl_base_command(server)?;
                command.args(["-sS", "--connect-timeout", "10", &url]);
                let output = command.output().context("failed to run curl")?;
                if !output.status.success() {
                    bail!(
                        "curl failed: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    );
                }
                println!("{} connectivity ok", server.protocol);
            }
        }
        other => bail!("unsupported protocol: {other}"),
    }

    Ok(())
}

fn list_server(server: &Server, path: Option<String>, json: bool) -> Result<()> {
    let requested = path.unwrap_or_else(|| ".".to_string());
    let mut entries = match server.protocol.as_str() {
        "local" => list_local(server, &requested)?,
        "sftp" => list_sftp(server, &requested)?,
        other => bail!("unsupported protocol: {other}"),
    };

    entries.sort_by(|left, right| {
        let left_dir = left.kind == "dir";
        let right_dir = right.kind == "dir";
        right_dir
            .cmp(&left_dir)
            .then_with(|| left.name.cmp(&right.name))
    });

    if json {
        let response = LsResponse {
            server: server.name.clone(),
            path: requested,
            entries,
        };
        println!("{}", serde_json::to_string(&response)?);
    } else {
        for entry in entries {
            if entry.kind == "dir" {
                println!("dir  {}", entry.name);
            } else {
                println!(
                    "file {}  {}  {}",
                    entry.name,
                    entry
                        .size
                        .map(|size| size.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    entry
                        .modified
                        .map(|modified| modified.to_string())
                        .unwrap_or_else(|| "-".to_string())
                );
            }
        }
    }

    Ok(())
}

fn list_local(server: &Server, path: &str) -> Result<Vec<LsEntry>> {
    let directory = PathBuf::from(&server.remote_root).join(path);
    let read = fs::read_dir(&directory)
        .with_context(|| format!("failed to list {}", directory.display()))?;

    let mut entries = Vec::new();
    for entry in read {
        let entry = entry.with_context(|| format!("failed to read {}", directory.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "." || name == ".." {
            continue;
        }
        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to stat {}", entry.path().display()))?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs());
        entries.push(LsEntry {
            name,
            kind: if metadata.is_dir() {
                "dir".to_string()
            } else {
                "file".to_string()
            },
            size: Some(metadata.len()),
            modified,
        });
    }

    Ok(entries)
}

fn list_sftp(server: &Server, path: &str) -> Result<Vec<LsEntry>> {
    let remote_path = build_remote_path(&server.remote_root, path);
    let output = run_ssh_capture(server, &format!("ls -1p -- {}", shell_quote(&remote_path)))
        .with_context(|| format!("failed to list {}", remote_path))?;

    let mut entries = Vec::new();
    for line in output.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let is_dir = line.ends_with('/');
        let name = line.trim_end_matches('/').to_string();
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }
        entries.push(LsEntry {
            name,
            kind: if is_dir {
                "dir".to_string()
            } else {
                "file".to_string()
            },
            size: None,
            modified: None,
        });
    }

    Ok(entries)
}

fn build_remote_path(remote_root: &str, path: &str) -> String {
    let root = remote_root.trim_end_matches('/');
    let relative = path.trim_start_matches('/');
    let combined = if root.is_empty() {
        format!("/{relative}")
    } else if relative.is_empty() {
        root.to_string()
    } else {
        format!("{root}/{relative}")
    };

    let mut normalized = String::with_capacity(combined.len());
    let mut previous_slash = false;
    for character in combined.chars() {
        if character == '/' {
            if !previous_slash {
                normalized.push(character);
            }
            previous_slash = true;
        } else {
            normalized.push(character);
            previous_slash = false;
        }
    }
    normalized
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn normalize_relative_path(path: &str) -> Result<PathBuf> {
    if path.starts_with('/') {
        bail!("--path must be relative, got absolute path: {path}");
    }
    let mut normalized = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir => bail!("--path must not contain '..': {path}"),
            Component::RootDir | Component::Prefix(_) => {
                bail!("--path must be relative: {path}")
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        bail!("--path must not be empty");
    }
    Ok(normalized)
}

fn resolve_local_remote_path(remote_root: &str, relative: &Path) -> Result<PathBuf> {
    let root = PathBuf::from(remote_root);
    let joined = root.join(relative);
    let canonical_root = fs::canonicalize(&root)
        .with_context(|| format!("remote root does not exist: {}", root.display()))?;

    let mut existing = joined.as_path();
    while !existing.exists() {
        existing = existing
            .parent()
            .with_context(|| format!("cannot resolve {}", joined.display()))?;
    }
    let canonical_existing = fs::canonicalize(existing)
        .with_context(|| format!("cannot resolve {}", existing.display()))?;
    if !canonical_existing.starts_with(&canonical_root) {
        bail!("--path escapes remote root: {relative:?}");
    }
    Ok(joined)
}

fn absolute_local_path(to: &Path) -> Result<PathBuf> {
    if to.is_absolute() {
        Ok(to.to_path_buf())
    } else {
        Ok(env::current_dir()?.join(to))
    }
}

fn sftp_remote_path(server: &Server, relative: &Path) -> String {
    format!(
        "{}/{}",
        server.remote_root.trim_end_matches('/'),
        relative.to_string_lossy().replace('\\', "/")
    )
}

fn download_server(server: &Server, path: &str, to: &Path, json: bool) -> Result<()> {
    let relative = normalize_relative_path(path)?;
    let target = absolute_local_path(to)?;
    if target.is_dir() {
        bail!("refusing to overwrite directory: {}", target.display());
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    match server.protocol.as_str() {
        "local" => {
            let source = resolve_local_remote_path(&server.remote_root, &relative)?;
            if !source.exists() {
                bail!("remote path not found: {path}");
            }
            if source.is_dir() {
                bail!("cannot download a directory: {path}");
            }
            fs::copy(&source, &target).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    source.display(),
                    target.display()
                )
            })?;
        }
        "sftp" => {
            let remote = sftp_remote_path(server, &relative);
            let batch = format!(
                "get \"{remote}\" \"{}\"\n",
                target.to_string_lossy().replace('"', "\\\"")
            );
            run_sftp(server, &batch)?;
        }
        other => bail!("unsupported protocol: {other}"),
    }

    let bytes = fs::metadata(&target)
        .with_context(|| format!("failed to stat {}", target.display()))?
        .len();
    if json {
        let response = DownloadResponse {
            server: server.name.clone(),
            path: path.to_string(),
            to: target.to_string_lossy().into_owned(),
            bytes,
        };
        println!("{}", serde_json::to_string(&response)?);
    } else {
        println!("downloaded {path} -> {} ({bytes} bytes)", target.display());
    }
    Ok(())
}

fn delete_server(
    server: &Server,
    path: &str,
    recursive: bool,
    allow_delete: bool,
    json: bool,
) -> Result<()> {
    if !allow_delete {
        bail!("refusing to delete {path}: pass --allow-delete");
    }
    let relative = normalize_relative_path(path)?;

    match server.protocol.as_str() {
        "local" => {
            let target = resolve_local_remote_path(&server.remote_root, &relative)?;
            let metadata = fs::symlink_metadata(&target)
                .with_context(|| format!("remote path not found: {path}"))?;
            if metadata.is_dir() {
                if !recursive {
                    bail!("refusing to delete directory {path}: pass --recursive");
                }
                fs::remove_dir_all(&target)
                    .with_context(|| format!("failed to delete {}", target.display()))?;
            } else {
                fs::remove_file(&target)
                    .with_context(|| format!("failed to delete {}", target.display()))?;
            }
        }
        "sftp" => {
            if recursive {
                bail!("recursive delete is not supported over sftp; delete files individually");
            }
            let remote = sftp_remote_path(server, &relative);
            let probe = run_ssh_capture(
                server,
                &format!(
                    "if [ -d {0} ]; then echo dir; elif [ -e {0} ]; then echo file; else echo missing; fi",
                    shell_quote(&remote)
                ),
            )?;
            match probe.trim() {
                "dir" => run_sftp(server, &format!("rmdir \"{remote}\"\n"))?,
                "file" => run_sftp(server, &format!("rm \"{remote}\"\n"))?,
                _ => bail!("remote path not found: {path}"),
            }
        }
        other => bail!("unsupported protocol: {other}"),
    }

    if json {
        let response = DeleteResponse {
            server: server.name.clone(),
            path: path.to_string(),
            deleted: true,
        };
        println!("{}", serde_json::to_string(&response)?);
    } else {
        println!("deleted {path}");
    }
    Ok(())
}

fn sync_server(server: &Server, project_root: &Path, dry_run: bool) -> Result<()> {
    let root = local_root(server, project_root);
    let files = collect_files(&root, &server.exclude)?;
    println!("{} files to deploy", files.len());

    match server.protocol.as_str() {
        "local" => {
            let destination = PathBuf::from(&server.remote_root);
            for file in files {
                let relative = file.strip_prefix(&root)?;
                let target = destination.join(relative);
                if dry_run {
                    println!("copy {} -> {}", file.display(), target.display());
                    continue;
                }
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&file, &target)?;
            }
        }
        "sftp" => {
            let mut batch = String::new();
            for file in &files {
                let relative = file.strip_prefix(&root)?;
                append_sftp_put(&mut batch, server, relative, file);
            }
            if dry_run {
                println!("{batch}");
            } else {
                run_sftp(server, &batch)?;
            }
        }
        other => bail!("unsupported protocol: {other}"),
    }

    Ok(())
}

fn upload_server(
    server: &Server,
    project_root: &Path,
    file: &Path,
    dry_run: bool,
) -> Result<()> {
    let root = local_root(server, project_root);
    let absolute = if file.is_absolute() {
        file.to_path_buf()
    } else {
        root.join(file)
    };
    let relative = absolute
        .strip_prefix(&root)
        .with_context(|| format!("{} is outside the local root", absolute.display()))?;

    if is_excluded(relative, &server.exclude) {
        bail!("{} is excluded by deploy config", relative.display());
    }

    upload_file(server, relative, &absolute, dry_run)
}

fn upload_file(server: &Server, relative: &Path, absolute: &Path, dry_run: bool) -> Result<()> {
    match server.protocol.as_str() {
        "local" => {
            let target = PathBuf::from(&server.remote_root).join(relative);
            if dry_run {
                println!("copy {} -> {}", absolute.display(), target.display());
                return Ok(());
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(absolute, &target)?;
            println!("uploaded {}", relative.display());
        }
        "sftp" => {
            let mut batch = String::new();
            append_sftp_put(&mut batch, server, relative, absolute);
            if dry_run {
                println!("{batch}");
                return Ok(());
            }
            run_sftp(server, &batch)?;
            println!("uploaded {}", relative.display());
        }
        other => bail!("unsupported protocol: {other}"),
    }
    Ok(())
}

fn append_sftp_put(batch: &mut String, server: &Server, relative: &Path, absolute: &Path) {
    let remote_root = server.remote_root.trim_end_matches('/');
    let mut directory = PathBuf::new();
    if let Some(parent) = relative.parent() {
        for component in parent.components() {
            directory.push(component);
            batch.push_str(&format!(
                "-mkdir \"{remote_root}/{}\"\n",
                directory.to_string_lossy()
            ));
        }
    }
    let remote_file = format!(
        "{remote_root}/{}",
        relative.to_string_lossy().replace('\\', "/")
    );
    batch.push_str(&format!(
        "put \"{}\" \"{}\"\n",
        absolute.display(),
        remote_file
    ));
}

fn watch_server(server: &Server, project_root: &Path, interval: u64) -> Result<()> {
    let root = local_root(server, project_root);
    let mut known = snapshot(&root, &server.exclude)?;
    println!(
        "watching {} -> {} every {}s (Ctrl-C to stop)",
        root.display(),
        server.remote_root,
        interval
    );

    loop {
        thread::sleep(Duration::from_secs(interval));
        let current = snapshot(&root, &server.exclude)?;
        for (path, stamp) in &current {
            if known.get(path) == Some(stamp) {
                continue;
            }
            let relative = path.strip_prefix(&root)?;
            if let Err(error) = upload_file(server, relative, path, false) {
                eprintln!("upload failed for {}: {error:#}", relative.display());
            }
        }
        known = current;
    }
}

fn snapshot(root: &Path, exclude: &[String]) -> Result<HashMap<PathBuf, (u64, u64)>> {
    let mut result = HashMap::new();
    for file in collect_files(root, exclude)? {
        let metadata = fs::metadata(&file)?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        result.insert(file, (modified, metadata.len()));
    }
    Ok(result)
}

fn collect_files(root: &Path, exclude: &[String]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    visit(root, root, exclude, &mut files)?;
    files.sort();
    Ok(files)
}

fn visit(
    root: &Path,
    directory: &Path,
    exclude: &[String],
    files: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("failed to read {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(&path);
        if is_excluded(relative, exclude) {
            continue;
        }
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            visit(root, &path, exclude, files)?;
        } else if metadata.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn is_excluded(relative: &Path, patterns: &[String]) -> bool {
    let text = relative.to_string_lossy().replace('\\', "/");
    patterns.iter().any(|pattern| {
        if let Some(suffix) = pattern.strip_prefix("*.") {
            text.ends_with(&format!(".{suffix}"))
        } else if let Some(prefix) = pattern.strip_suffix('/') {
            text == prefix || text.starts_with(&format!("{prefix}/"))
        } else {
            text == *pattern || text.starts_with(&format!("{pattern}/"))
        }
    })
}

fn run_sftp(server: &Server, batch: &str) -> Result<()> {
    let host = server.host.as_deref().context("sftp requires host")?;
    let port = server.port.unwrap_or(22).to_string();
    let destination = match server.user.as_deref() {
        Some(user) => format!("{user}@{host}"),
        None => host.to_string(),
    };
    let secret = server.ssh_secret();

    let mut command = ssh_base_command(server, "sftp")?;
    command
        .args([
            "-o",
            "ConnectTimeout=10",
            "-P",
            port.as_str(),
            "-b",
            "-",
            destination.as_str(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .context("failed to run sftp")?;

    child
        .stdin
        .as_mut()
        .context("sftp stdin unavailable")?
        .write_all(batch.as_bytes())?;

    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "sftp failed: {}",
            scrub(&String::from_utf8_lossy(&output.stderr).trim(), secret)
        );
    }
    Ok(())
}

fn run_ssh(server: &Server, remote_command: &str) -> Result<()> {
    run_ssh_capture(server, remote_command).map(|_| ())
}

fn run_ssh_capture(server: &Server, remote_command: &str) -> Result<String> {
    let host = server.host.as_deref().context("sftp requires host")?;
    let port = server.port.unwrap_or(22).to_string();
    let destination = match server.user.as_deref() {
        Some(user) => format!("{user}@{host}"),
        None => host.to_string(),
    };
    let secret = server.ssh_secret();

    let mut command = ssh_base_command(server, "ssh")?;
    command.args([
        "-o",
        "ConnectTimeout=10",
        "-p",
        port.as_str(),
        destination.as_str(),
        remote_command,
    ]);

    let output = command.output().context("failed to run ssh")?;

    if !output.status.success() {
        bail!(
            "ssh failed: {}",
            scrub(&String::from_utf8_lossy(&output.stderr).trim(), secret)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn normalize_accepts_relative_files() {
        assert_eq!(
            normalize_relative_path("index.php").unwrap(),
            PathBuf::from("index.php")
        );
        assert_eq!(
            normalize_relative_path("./subdir/nested.php").unwrap(),
            PathBuf::from("subdir/nested.php")
        );
        assert_eq!(
            normalize_relative_path("a//b/./c").unwrap(),
            PathBuf::from("a/b/c")
        );
    }

    #[test]
    fn normalize_rejects_absolute_paths() {
        assert!(normalize_relative_path("/etc/passwd").is_err());
        assert!(normalize_relative_path("//etc/passwd").is_err());
    }

    #[test]
    fn normalize_rejects_parent_traversal() {
        assert!(normalize_relative_path("../etc/passwd").is_err());
        assert!(normalize_relative_path("subdir/../../etc/passwd").is_err());
        assert!(normalize_relative_path("a/../b").is_err());
    }

    #[test]
    fn normalize_rejects_empty_path() {
        assert!(normalize_relative_path("").is_err());
        assert!(normalize_relative_path(".").is_err());
    }

    #[test]
    fn resolve_local_stays_under_remote_root() {
        let root = env::temp_dir().join(format!("php-deploy-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("subdir")).unwrap();
        fs::write(root.join("subdir/nested.php"), b"ok").unwrap();

        let resolved =
            resolve_local_remote_path(root.to_str().unwrap(), Path::new("subdir/nested.php"))
                .unwrap();
        assert_eq!(resolved, root.join("subdir/nested.php"));

        let outside = root.parent().unwrap().join("elsewhere");
        fs::create_dir_all(&outside).unwrap();
        assert!(resolve_local_remote_path(
            root.to_str().unwrap(),
            Path::new("../elsewhere")
        )
        .is_err());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn parse_download_arguments() {
        let args = Args::parse(strings(&[
            "--server",
            "sftp",
            "download",
            "--path",
            "index.php",
            "--to",
            "/tmp/remote-download/index.php",
            "--json",
        ]))
        .unwrap();
        assert_eq!(args.command.as_deref(), Some("download"));
        assert_eq!(args.server_name.as_deref(), Some("sftp"));
        assert_eq!(args.remote_path.as_deref(), Some("index.php"));
        assert_eq!(
            args.to.unwrap(),
            PathBuf::from("/tmp/remote-download/index.php")
        );
        assert!(args.json);
    }

    #[test]
    fn parse_delete_arguments() {
        let args = Args::parse(strings(&[
            "delete", "--path", "subdir", "--recursive", "--allow-delete", "--json",
        ]))
        .unwrap();
        assert_eq!(args.command.as_deref(), Some("delete"));
        assert_eq!(args.remote_path.as_deref(), Some("subdir"));
        assert!(args.recursive);
        assert!(args.allow_delete);
        assert!(args.json);
    }

    #[test]
    fn parse_rejects_missing_option_value() {
        assert!(Args::parse(strings(&["download", "--path"])).is_err());
        assert!(Args::parse(strings(&["download", "--to"])).is_err());
    }

    #[test]
    fn parse_rejects_unknown_flag_and_extra_command() {
        assert!(Args::parse(strings(&["download", "--bogus"])).is_err());
        assert!(Args::parse(strings(&["download", "delete"])).is_err());
    }

    #[test]
    fn help_flag_is_forwarded() {
        let args = Args::parse(strings(&["--help"])).unwrap();
        assert!(args.help);
    }

    fn server(json: &str) -> Server {
        serde_json::from_str(json).unwrap()
    }

    fn argv(command: &Command) -> (String, Vec<String>) {
        (
            command.get_program().to_string_lossy().into_owned(),
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect(),
        )
    }

    /// A hermetic directory holding an executable dummy `sshpass` so the
    /// password-mode tests do not depend on a real installation.
    fn fake_sshpass_dir() -> PathBuf {
        let dir = env::temp_dir().join(format!("php-deploy-test-bin-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("sshpass");
        if !fake.exists() {
            fs::write(&fake, b"#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        dir
    }

    #[test]
    fn resolved_auth_infers_mode_from_credentials() {
        // Explicit ssh_auth wins.
        assert_eq!(
            server(
                r#"{"name":"s","protocol":"sftp","host":"h","remote_root":"/r",
                    "ssh_auth":"agent","ssh_password":"pw"}"#
            )
            .resolved_auth(),
            SshAuth::Agent
        );
        // Without ssh_auth, credentials decide.
        assert_eq!(
            server(
                r#"{"name":"s","protocol":"sftp","host":"h","remote_root":"/r",
                    "ssh_password":"pw"}"#
            )
            .resolved_auth(),
            SshAuth::Password
        );
        assert_eq!(
            server(
                r#"{"name":"s","protocol":"sftp","host":"h","remote_root":"/r",
                    "ssh_key_path":"~/.ssh/id_ed25519"}"#
            )
            .resolved_auth(),
            SshAuth::Key
        );
        // The fixture default stays agent.
        assert_eq!(
            server(
                r#"{"name":"s","protocol":"sftp","host":"h","remote_root":"/r"}"#
            )
            .resolved_auth(),
            SshAuth::Agent
        );
    }

    #[test]
    fn ssh_base_command_agent_mode_argv() {
        let s = server(r#"{"name":"s","protocol":"sftp","host":"h","remote_root":"/r"}"#);
        let (program, args) = argv(&ssh_base_command_with(&s, "ssh", None).unwrap());
        assert_eq!(program, "ssh");
        assert_eq!(args, vec!["-o", "BatchMode=yes"]);
    }

    #[test]
    fn ssh_base_command_key_mode_argv() {
        let s = server(
            r#"{"name":"s","protocol":"sftp","host":"h","remote_root":"/r",
                "ssh_auth":"key","ssh_key_path":"/home/deploy/.ssh/id_rsa"}"#,
        );
        let (program, args) = argv(&ssh_base_command_with(&s, "sftp", None).unwrap());
        assert_eq!(program, "sftp");
        assert_eq!(
            args,
            vec!["-i", "/home/deploy/.ssh/id_rsa", "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes"]
        );
    }

    #[test]
    fn ssh_base_command_password_mode_argv_prefixes_sshpass() {
        let s = server(
            r#"{"name":"s","protocol":"sftp","host":"h","remote_root":"/r",
                "ssh_auth":"password","ssh_password":"hunter2"}"#,
        );
        let path = fake_sshpass_dir();
        let (program, args) = argv(&ssh_base_command_with(&s, "ssh", Some(path.as_os_str())).unwrap());
        assert_eq!(program, "sshpass");
        assert_eq!(
            args,
            vec![
                "-p",
                "hunter2",
                "ssh",
                "-o",
                "PreferredAuthentications=password",
                "-o",
                "NumberOfPasswordPrompts=1",
            ]
        );
    }

    #[test]
    fn ssh_base_command_password_mode_without_sshpass_gives_guidance() {
        let s = server(
            r#"{"name":"s","protocol":"sftp","host":"h","remote_root":"/r",
                "ssh_auth":"password","ssh_password":"hunter2"}"#,
        );
        let error = ssh_base_command_with(&s, "ssh", Some(OsStr::new("/nonexistent-deploy-path")))
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "sshpass not installed — use key or agent authentication"
        );
    }

    #[test]
    fn ssh_base_command_requires_credential_fields() {
        let s = server(
            r#"{"name":"s","protocol":"sftp","host":"h","remote_root":"/r","ssh_auth":"key"}"#,
        );
        let error = ssh_base_command_with(&s, "ssh", None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("auth=key") || error.contains("ssh_key_path"), "{error}");

        let s = server(
            r#"{"name":"s","protocol":"sftp","host":"h","remote_root":"/r","ssh_auth":"password"}"#,
        );
        let path = fake_sshpass_dir();
        let error = ssh_base_command_with(&s, "ssh", Some(path.as_os_str()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("ssh_password"), "{error}");
    }

    #[test]
    fn ssh_auth_warnings_flag_missing_sshpass() {
        let s = server(
            r#"{"name":"s","protocol":"sftp","host":"h","remote_root":"/r",
                "ssh_auth":"password","ssh_password":"hunter2"}"#,
        );
        let warnings = ssh_auth_warnings_with(&s, Some(OsStr::new("/nonexistent-deploy-path")));
        assert_eq!(
            warnings,
            vec!["sshpass not installed — key or agent authentication required".to_string()]
        );

        // With a usable sshpass the warning disappears.
        let path = fake_sshpass_dir();
        assert!(ssh_auth_warnings_with(&s, Some(path.as_os_str())).is_empty());

        // Agent mode never warns.
        let s = server(r#"{"name":"s","protocol":"sftp","host":"h","remote_root":"/r"}"#);
        assert!(ssh_auth_warnings_with(&s, Some(OsStr::new("/nonexistent-deploy-path"))).is_empty());
    }

    #[test]
    fn scrub_removes_password_from_error_text() {
        assert_eq!(
            scrub("sshpass -p hunter2 failed: bad", Some("hunter2")),
            "sshpass -p *** failed: bad"
        );
        assert_eq!(scrub("untouched", None), "untouched");
        assert_eq!(scrub("untouched", Some("")), "untouched");
    }

    #[test]
    fn invalid_ssh_auth_value_is_rejected_at_load() {
        let error = serde_json::from_str::<Server>(
            r#"{"name":"s","protocol":"sftp","host":"h","remote_root":"/r","ssh_auth":"agent2"}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("invalid ssh_auth `agent2`"), "{error}");
    }

    #[test]
    fn version_flag_is_forwarded() {
        let args = Args::parse(strings(&["--version"])).unwrap();
        assert!(args.version);
    }

    #[test]
    fn connection_defaults_when_fields_absent() {
        let s = server(r#"{"name":"f","protocol":"ftp","host":"h","remote_root":"/pub"}"#);
        assert!(s.passive_mode);
        assert!(!s.show_hidden);
        assert!(!s.use_mlsd);
        assert!(!s.list_compat);
        assert_eq!(s.max_connections, 5);
        assert!(s.keep_alive_enabled);
        assert_eq!(s.keep_alive_seconds, 300);
        assert_eq!(s.encoding, "UTF-8");
    }

    #[test]
    fn active_mode_appends_dash_P_dash_for_ftp_and_ftps() {
        for protocol in ["ftp", "ftps"] {
            let s = server(&format!(
                r#"{{"name":"f","protocol":"{protocol}","host":"h","remote_root":"/pub",
                    "passive_mode":false}}"#
            ));
            let (program, args) = argv(&curl_base_command(&s).unwrap());
            assert_eq!(program, "curl");
            assert_eq!(args, vec!["-P", "-"], "{protocol}");
        }
    }

    #[test]
    fn passive_mode_builds_clean_curl_argv() {
        for json in [
            r#"{"name":"f","protocol":"ftp","host":"h","remote_root":"/pub"}"#,
            r#"{"name":"f","protocol":"ftps","host":"h","remote_root":"/pub","passive_mode":true}"#,
        ] {
            let (program, args) = argv(&curl_base_command(&server(json)).unwrap());
            assert_eq!(program, "curl");
            assert!(args.is_empty(), "{args:?}");
        }
    }

    #[test]
    fn active_mode_is_ignored_outside_ftp() {
        let s = server(
            r#"{"name":"s","protocol":"sftp","host":"h","remote_root":"/r","passive_mode":false}"#,
        );
        let (_, args) = argv(&ssh_base_command_with(&s, "ssh", None).unwrap());
        assert!(!args.contains(&"-P".to_string()));
        let error = curl_base_command(&s).unwrap_err().to_string();
        assert_eq!(error, "curl transport requires ftp or ftps, got sftp");
    }

    #[test]
    fn ftp_url_keeps_scheme_optional_port_and_requires_host() {
        let s = server(r#"{"name":"f","protocol":"ftp","host":"h","remote_root":"/pub"}"#);
        assert_eq!(ftp_url(&s, "").unwrap(), "ftp://h/");
        assert_eq!(ftp_url(&s, "pub/files").unwrap(), "ftp://h/pub/files");

        let s = server(
            r#"{"name":"f","protocol":"ftps","host":"127.0.0.1","port":2121,"remote_root":"/pub"}"#,
        );
        assert_eq!(ftp_url(&s, "").unwrap(), "ftps://127.0.0.1:2121/");

        let s = server(r#"{"name":"f","protocol":"ftp","remote_root":"/pub"}"#);
        let error = ftp_url(&s, "").unwrap_err().to_string();
        assert_eq!(error, "ftp requires host");
    }

    #[test]
    fn connection_fields_round_trip() {
        let original: Server = serde_json::from_str(
            r#"{
            "name":"f","protocol":"ftps","host":"h","port":990,"user":"u",
            "remote_root":"/pub","local_root":".","exclude":["vendor/"],
            "passive_mode":false,"show_hidden":true,"use_mlsd":true,
            "list_compat":true,"max_connections":9,
            "keep_alive_enabled":false,"keep_alive_seconds":30,
            "encoding":"Windows-1251"
        }"#,
        )
        .unwrap();
        let serialized = serde_json::to_string(&original).unwrap();
        let reparsed: Server = serde_json::from_str(&serialized).unwrap();

        assert_eq!(original.name, reparsed.name);
        assert_eq!(original.protocol, reparsed.protocol);
        assert_eq!(original.host, reparsed.host);
        assert_eq!(original.port, reparsed.port);
        assert_eq!(original.user, reparsed.user);
        assert_eq!(original.remote_root, reparsed.remote_root);
        assert_eq!(original.local_root, reparsed.local_root);
        assert_eq!(original.exclude, reparsed.exclude);
        assert_eq!(original.passive_mode, reparsed.passive_mode);
        assert_eq!(original.show_hidden, reparsed.show_hidden);
        assert_eq!(original.use_mlsd, reparsed.use_mlsd);
        assert_eq!(original.list_compat, reparsed.list_compat);
        assert_eq!(original.max_connections, reparsed.max_connections);
        assert_eq!(original.keep_alive_enabled, reparsed.keep_alive_enabled);
        assert_eq!(original.keep_alive_seconds, reparsed.keep_alive_seconds);
        assert_eq!(original.encoding, reparsed.encoding);

        // The save path must actually write the fields, not drop them.
        for marker in [
            "\"passive_mode\":false",
            "\"show_hidden\":true",
            "\"use_mlsd\":true",
            "\"list_compat\":true",
            "\"max_connections\":9",
            "\"keep_alive_enabled\":false",
            "\"keep_alive_seconds\":30",
            "\"encoding\":\"Windows-1251\"",
        ] {
            assert!(serialized.contains(marker), "missing {marker} in {serialized}");
        }
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        let s = server(
            r#"{"name":"f","protocol":"ftp","host":"h","remote_root":"/pub",
                "visible":true,"use_rsync":true,"web_url":"http://h",
                "future_setting":{"deep":[1,2]},"passive_mode":false}"#,
        );
        assert!(!s.passive_mode);
        assert_eq!(s.max_connections, 5);
    }
}
