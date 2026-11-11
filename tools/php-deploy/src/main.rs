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
    from: Option<PathBuf>,
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
                "--server" | "--name" => parsed.server_name = Some(args.next().context("--server name")?),
                "--file" => parsed.file = Some(PathBuf::from(args.next().context("--file path")?)),
                "--path" => parsed.remote_path = Some(args.next().context("--path REMOTE_PATH")?),
                "--to" => parsed.to = Some(PathBuf::from(args.next().context("--to LOCAL_PATH")?)),
                "--from" => parsed.from = Some(PathBuf::from(args.next().context("--from LOCAL_PATH")?)),
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
    /// Empty string means "not specified" (see `validate`); ftp/ftps treats an
    /// empty root as `/` when building URLs.
    #[serde(default)]
    remote_root: String,
    /// Password for the curl-based transports (ftp/ftps/webdav/webdavs); the
    /// sftp transport keeps using `ssh_password`. Falls back to
    /// `ssh_password` when unset so a single-panel password still works.
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    use_rsync: bool,
    #[serde(default)]
    rsync_args: Option<String>,
    #[serde(default)]
    rsync_path: Option<String>,
    #[serde(default)]
    rsync_delete: Option<bool>,
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

    /// Curl secret (`password` with fallback to `ssh_password`) for
    /// ftp/ftps/webdav/webdavs; also used to scrub curl error text.
    fn curl_secret(&self) -> Option<&str> {
        self.password
            .as_deref()
            .or(self.ssh_password.as_deref())
            .filter(|p| !p.is_empty())
    }
}

/// Only the `--ssl-reqd` flag for ftps; passive mode lives in
/// `curl_base_command` (called by `check` and reused here).
fn curl_transport_command(server: &Server) -> Result<Command> {
    let mut command = curl_base_command(server)?;
    if server.protocol == "ftps" {
        command.arg("--ssl-reqd");
    }
    Ok(command)
}

/// Appends `-u user:password` when a user is configured. `anonymous` without
/// a password still gets the flag (curl sends an empty password explicitly).
fn append_curl_auth(command: &mut Command, user: Option<&str>, secret: Option<&str>) {
    if let Some(user) = user.filter(|user| !user.is_empty()) {
        command.arg("-u").arg(format!("{}:{}", user, secret.unwrap_or("")));
    }
}

/// The directory path (no leading slash, no trailing slash) formed from the
/// server root and a relative request path. Empty root + empty path yields an
/// empty string, i.e. the transport's root directory.
fn relative_dir_path(remote_root: &str, relative: &str) -> String {
    let root = remote_root.trim_matches('/');
    let rel = relative.trim();
    if rel.is_empty() || rel == "." {
        return root.to_string();
    }
    let rel = rel.trim_matches('/');
    if root.is_empty() {
        rel.replace("//", "/")
    } else {
        format!("{root}/{rel}").replace("//", "/")
    }
}

/// Builds a single command that maps a `Vec` of arguments onto the
/// transport prefix, avoiding verbose arg chains at call sites.
fn with_args(mut command: Command, args: &[&str]) -> Command {
    command.args(args);
    command
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

// ---------------------------------------------------------------- ftp/ftps

/// The absolute URL path for a listing: `<root>/<path>/` with a trailing
/// slash so curl LISTs a directory. An empty root falls back to `/`.
fn ftp_list_url(server: &Server, relative: &str) -> Result<String> {
    let dir = relative_dir_path(&server.remote_root, relative);
    let path = if dir.is_empty() {
        "/".to_string()
    } else {
        format!("/{dir}/")
    };
    let host = server
        .host
        .as_deref()
        .filter(|host| !host.is_empty())
        .with_context(|| format!("{} requires host", server.protocol))?;
    let port = match server.port {
        Some(port) => format!(":{port}"),
        None => String::new(),
    };
    Ok(format!("{}://{}{port}{path}", server.protocol, host))
}

/// URL for a (non-listing) ftp operation on a file path: `<root>/<path>` with
/// no trailing slash.
fn ftp_file_url(server: &Server, relative: &str) -> Result<String> {
    let dir = relative_dir_path(&server.remote_root, relative);
    let path = if dir.is_empty() {
        "/".to_string()
    } else {
        format!("/{dir}")
    };
    let host = server
        .host
        .as_deref()
        .filter(|host| !host.is_empty())
        .with_context(|| format!("{} requires host", server.protocol))?;
    let port = match server.port {
        Some(port) => format!(":{port}"),
        None => String::new(),
    };
    Ok(format!("{}://{}{port}{path}", server.protocol, host))
}

/// Shared listing command flags per the transport contract.
fn ftp_list_command(server: &Server) -> Result<Command> {
    let mut command = with_args(
        curl_transport_command(server)?,
        &["-sS", "--connect-timeout", "5", "--max-time", "30"],
    );
    append_curl_auth(&mut command, server.user.as_deref(), server.curl_secret());
    Ok(command)
}

/// Captures a curl run; fails with a scrubbed error when curl exits nonzero.
fn run_curl(mut command: Command, secret: Option<&str>) -> Result<String> {
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("failed to run curl")?;
    if !output.status.success() {
        bail!(
            "curl failed: {}",
            scrub(&String::from_utf8_lossy(&output.stderr).trim(), secret)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn list_ftp(server: &Server, path: &str) -> Result<Vec<LsEntry>> {
    let mut command = ftp_list_command(server)?;
    command.arg(ftp_list_url(server, path)?);
    let listing = run_curl(command, server.curl_secret())
        .with_context(|| format!("failed to list {}", server.remote_root))?;
    Ok(parse_ftp_list(&listing))
}

fn parse_ftp_list(listing: &str) -> Vec<LsEntry> {
    let mut entries = Vec::new();
    for line in listing.lines() {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() || line.starts_with("total ") {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 9 {
            continue;
        }
        let permissions = fields[0];
        if permissions.len() < 10 {
            continue;
        }
        let size = fields[4].parse::<u64>().ok();
        let modified = parse_ftp_date(&fields[5..8]);
        // Beyond 8 fields the remainder is the name; a symlink line carries
        // a ` -> target` suffix which we strip.
        let raw = fields
            .iter()
            .skip(8)
            .copied()
            .collect::<Vec<&str>>()
            .join(" ");
        let name = match raw.split_once(" -> ") {
            // A symlink line: classify by a visibly-final '/' on the target.
            Some((name, target)) => {
                if name.is_empty() || name == "." || name == ".." {
                    continue;
                }
                let kind = if target.trim_end_matches('/').ends_with('/') || target.ends_with('/') {
                    "dir"
                } else {
                    "file"
                };
                entries.push(LsEntry {
                    name: name.to_string(),
                    kind: kind.to_string(),
                    size,
                    modified,
                });
                continue;
            }
            None => raw.clone(),
        };
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }
        let kind = if permissions.starts_with('d') {
            "dir"
        } else {
            "file"
        };
        entries.push(LsEntry {
            name,
            kind: kind.to_string(),
            size,
            modified,
        });
    }
    entries
}

const FTP_MONTHS: [(&str, u32); 12] = [
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("may", 5),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("oct", 10),
    ("nov", 11),
    ("dec", 12),
];

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn now_unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Parses the unix LIST date trio: `mmm dd yyyy` (absolute) or `mmm dd hh:mm`
/// (recent, within the last ~6 months). Returns unix seconds, or `None`.
fn parse_ftp_date(parts: &[&str]) -> Option<u64> {
    if parts.len() != 3 {
        return None;
    }
    let month = FTP_MONTHS
        .iter()
        .find(|(name, _)| *name == parts[0].to_lowercase())
        .map(|(_, month)| *month)?;
    let day: u32 = parts[1].trim().parse().ok()?;
    if let Ok(year) = parts[2].parse::<i64>() {
        if !(-1..10000).contains(&year) {
            return None;
        }
        return Some((days_from_civil(year, month, day) * 86400) as u64);
    }
    // `hh:mm` variant: assume the current year, stepping back one to sweep
    // past the year boundary (servers use this form for the last half-year
    // of files).
    let (hour, minute) = parts[2].split_once(':')?;
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    baseline_year_from(now_unix_seconds(), month, day, hour, minute)
}

/// Resolves the year for an `hh:mm` LIST entry given `now` unix seconds by
/// sweeping the two candidate years (this and previous) for the closest
/// timestamp that is not in the future.
fn baseline_year_from(now: i64, month: u32, day: u32, hour: u32, minute: u32) -> Option<u64> {
    let year_guess = year_of(now);
    for year in [year_guess, year_guess - 1] {
        let candidate = days_from_civil(year, month, day) * 86400
            + i64::from(hour) * 3600
            + i64::from(minute) * 60;
        // 366 days of tolerance both ways keeps the sweep cheap.
        if candidate <= now + 86_400 && candidate >= now - 367 * 86_400 {
            return Some(candidate as u64);
        }
    }
    None
}

/// Civil year of a unix timestamp (± leap tolerance: good to within a year,
/// which is enough for sweeping candidates).
fn year_of(now: i64) -> i64 {
    let days = now / 86400;
    let mut year = 1970;
    while days_from_civil(year + 1, 1, 1) <= days {
        year += 1;
    }
    year
}

fn download_ftp(server: &Server, relative: &Path, target: &Path) -> Result<()> {
    let mut command = with_args(
        curl_transport_command(server)?,
        &["-sS", "--connect-timeout", "5", "--max-time", "30", "-f"],
    );
    append_curl_auth(&mut command, server.user.as_deref(), server.curl_secret());
    command.arg(ftp_file_url(
        server,
        &relative.to_string_lossy().replace('\\', "/"),
    )?);
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("failed to run curl")?;
    if !output.status.success() {
        bail!(
            "curl failed: {}",
            scrub(
                &String::from_utf8_lossy(&output.stderr).trim(),
                server.curl_secret()
            )
        );
    }
    fs::write(target, &output.stdout)
        .with_context(|| format!("failed to write {}", target.display()))?;
    Ok(())
}

fn upload_ftp(server: &Server, relative: &Path, absolute: &Path) -> Result<()> {
    // --ftp-create-dirs is best-effort: curl creates missing parents during
    // the transfer and keeps uploading when creation fails.
    let mut command = with_args(
        curl_transport_command(server)?,
        &[
            "-sS",
            "--connect-timeout",
            "5",
            "--max-time",
            "300",
            "--ftp-create-dirs",
        ],
    );
    append_curl_auth(&mut command, server.user.as_deref(), server.curl_secret());
    command.args([
        "-T",
        absolute.to_string_lossy().as_ref(),
        ftp_file_url(
            server,
            &relative.to_string_lossy().replace('\\', "/"),
        )?
        .as_str(),
    ]);
    run_curl(command, server.curl_secret())?;
    Ok(())
}

fn delete_ftp(server: &Server, relative: &Path) -> Result<()> {
    let remote = relative.to_string_lossy().replace('\\', "/");
    let is_dir = ftp_entry_is_dir(server, relative)?;
    let mut command = with_args(
        curl_transport_command(server)?,
        &["-sS", "--connect-timeout", "5", "--max-time", "30"],
    );
    append_curl_auth(&mut command, server.user.as_deref(), server.curl_secret());
    if is_dir {
        command.args(["-X", "RMD"]);
    } else {
        command.args(["-X", "DELE"]);
    }
    command.arg(ftp_file_url(server, &remote)?);
    run_curl(command, server.curl_secret())?;
    Ok(())
}

/// Classifies the entry via a LIST of its parent: `dir` for a directory,
/// missing when the name is absent. Any parse failure is a missing entry.
fn ftp_entry_is_dir(server: &Server, relative: &Path) -> Result<bool> {
    let parent = match relative.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            parent.to_string_lossy().replace('\\', "/")
        }
        _ => ".".to_string(),
    };
    let mut command = ftp_list_command(server)?;
    command.arg(ftp_list_url(server, &parent)?);
    let listing = match run_curl(command, server.curl_secret()) {
        Ok(listing) => listing,
        Err(error) => return Err(error),
    };
    let name = relative
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .with_context(|| format!("cannot classify {relative:?}"))?;
    match parse_ftp_list(&listing)
        .into_iter()
        .find(|entry| entry.name == name)
    {
        Some(entry) => Ok(entry.kind == "dir"),
        None => bail!("remote path not found: {relative:?}"),
    }
}

// --------------------------------------------------------------- webdav

/// Scheme of a webdav/webdavs URL and whether it needs https.
fn webdav_scheme(protocol: &str) -> Result<&'static str> {
    match protocol {
        "webdav" => Ok("http"),
        "webdavs" => Ok("https"),
        other => bail!("webdav transport requires webdav or webdavs, got {other}"),
    }
}

/// URL for a webdav path under the server root; a trailing slash is appended
/// for directory listings.
fn webdav_url(server: &Server, relative: &str, trailing_slash: bool) -> Result<String> {
    let scheme = webdav_scheme(&server.protocol)?;
    let host = server
        .host
        .as_deref()
        .filter(|host| !host.is_empty())
        .with_context(|| format!("{} requires host", server.protocol))?;
    let port = match server.port {
        Some(port) => format!(":{port}"),
        None => String::new(),
    };
    let dir = relative_dir_path(&server.remote_root, relative);
    let mut url = if dir.is_empty() {
        format!("{scheme}://{host}{port}/")
    } else {
        format!("{scheme}://{host}{port}/{dir}")
    };
    if trailing_slash && !url.ends_with('/') {
        url.push('/');
    }
    Ok(url)
}

/// Curl prefix for webdav/webdavs: `-f` so HTTP failures exit nonzero.
fn webdav_command(server: &Server) -> Result<Command> {
    webdav_scheme(&server.protocol)?;
    let mut command = with_args(
        Command::new("curl"),
        &["-sS", "--connect-timeout", "5", "--max-time", "30", "-f"],
    );
    append_curl_auth(&mut command, server.user.as_deref(), server.curl_secret());
    Ok(command)
}

fn webdav_list_command(server: &Server) -> Result<Command> {
    let mut command = webdav_command(server)?;
    command.args(["-X", "PROPFIND", "-H", "Depth: 1"]);
    Ok(command)
}

fn list_webdav(server: &Server, path: &str) -> Result<Vec<LsEntry>> {
    let mut command = webdav_list_command(server)?;
    command.arg(webdav_url(server, path, true)?);
    let xml = run_curl(command, server.curl_secret())
        .with_context(|| format!("failed to list {}", server.remote_root))?;
    let request = webdav_url(server, path, true)?;
    let request_path = url_path(&request);
    Ok(parse_dav_listing(&xml, &request_path))
}

/// The path portion of a URL (after scheme://host[:port], without query).
fn url_path(url: &str) -> String {
    let after_scheme = url
        .find("://")
        .map(|index| &url[index + 3..])
        .unwrap_or(url);
    let after_host = after_scheme
        .find('/')
        .map(|index| &after_scheme[index..])
        .unwrap_or("/");
    let path = after_host
        .split('?')
        .next()
        .unwrap_or(after_host)
        .to_string();
    path.trim_end_matches('/').to_string()
}

fn upload_webdav(server: &Server, relative: &Path, absolute: &Path) -> Result<()> {
    let web_unit = relative.to_string_lossy().replace('\\', "/");
    // Best-effort: MKCOL for each missing ancestor (shallowest first);
    // failures (typically 405 "collection exists") are ignored.
    for dir in webdav_parent_dirs(&web_unit) {
        let mut command = webdav_command(server)?;
        command.args(["-X", "MKCOL"]);
        command.arg(webdav_url(server, &dir, false)?);
        let _ = run_curl(command, server.curl_secret());
    }

    let mut command = webdav_command(server)?;
    command.args(["-T", absolute.to_string_lossy().as_ref()]);
    command.arg(webdav_url(server, &web_unit, false)?);
    run_curl(command, server.curl_secret())?;
    Ok(())
}

/// Collects the MKCOL paths needed for a nested upload; `a/b/c.php` yields
/// `a` then `a/b` as server-relative directory prefixes.
fn webdav_parent_dirs(relative: &str) -> Vec<String> {
    let mut prefix = Path::new(relative);
    let mut dirs = Vec::new();
    prefix = prefix.parent().unwrap_or_else(|| Path::new(""));
    while !prefix.as_os_str().is_empty() {
        dirs.push(prefix.to_string_lossy().into_owned());
        prefix = prefix.parent().unwrap_or_else(|| Path::new(""));
    }
    dirs.reverse();
    dirs
}

fn download_webdav(server: &Server, relative: &Path, target: &Path) -> Result<()> {
    let mut command = webdav_command(server)?;
    command.arg(webdav_url(
        server,
        &relative.to_string_lossy().replace('\\', "/"),
        false,
    )?);
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("failed to run curl")?;
    if !output.status.success() {
        bail!(
            "curl failed: {}",
            scrub(
                &String::from_utf8_lossy(&output.stderr).trim(),
                server.curl_secret()
            )
        );
    }
    fs::write(target, &output.stdout)
        .with_context(|| format!("failed to write {}", target.display()))?;
    Ok(())
}

fn delete_webdav(server: &Server, relative: &Path) -> Result<()> {
    let mut command = webdav_command(server)?;
    command.args(["-X", "DELETE"]);
    command.arg(webdav_url(
        server,
        &relative.to_string_lossy().replace('\\', "/"),
        false,
    )?);
    run_curl(command, server.curl_secret())?;
    Ok(())
}

/// Minimal tolerant PROPFIND parser. Handles `D:`-style, `d:`-style, and
/// unprefixed element names, plus generic prefixes (`ms:`, `Dav:`, ...).
/// Skips the response block matching `request_path` (the collection itself)
/// and %-decodes hrefs. Returns entries sorted by the caller.
fn parse_dav_listing(xml: &str, request_path: &str) -> Vec<LsEntry> {
    let lowered = xml.to_ascii_lowercase();
    let mut entries = Vec::new();
    let bytes = lowered.as_bytes();
    let mut index = 0;
    while index < lowered.len() {
        if bytes[index] != b'<' {
            index += 1;
            continue;
        }
        // Read the tag name after '<'.
        let name_start = index + 1;
        let mut name_end = name_start;
        while name_end < lowered.len() {
            let byte = bytes[name_end];
            if byte.is_ascii_alphanumeric() || byte == b':' || byte == b'.' || byte == b'-' || byte == b'_' {
                name_end += 1;
            } else {
                break;
            }
        }
        let tag = &lowered[name_start..name_end];
        if tag.is_empty() {
            index += 1;
            continue;
        }
        // Only `<…response>` blocks carry entries; every other tag (href
        // outside a block, multistatus, propstat, …) is skipped.
        if !tag.ends_with("response") {
            index = name_end;
            continue;
        }
        // A response block: locate its closing tag (same raw name).
        let body_end = find_closing(&lowered, index, tag);
        let body_end = match body_end {
            Some(end) => end,
            None => break,
        };
        let block = &xml[index..body_end];
        if tag.ends_with("response") {
            if let Some(entry) = parse_dav_response(block, request_path) {
                entries.push(entry);
            }
            index = body_end;
            continue;
        }
        index = body_end;
    }
    entries
}

fn find_closing(text: &str, from: usize, tag: &str) -> Option<usize> {
    let needle = format!("</{tag}>");
    text[from..].find(&needle).map(|offset| from + offset + needle.len())
}

/// Parses one `<...response>` block into an entry, or `None` for the self
/// block / malformed hrefs.
fn parse_dav_response(block: &str, request_path: &str) -> Option<LsEntry> {
    let href = block_tag_value(block, "href")?;
    let href_path = url_path(href.trim());
    if href_path == request_path {
        return None;
    }
    let decoded = decode_percent(&href_path);
    let name = decoded
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("");
    if name.is_empty() {
        return None;
    }
    let is_dir = block_tag_contains(block, "collection");
    let size = block_tag_value(block, "getcontentlength")
        .and_then(|text| text.trim().trim().parse::<u64>().ok());
    Some(LsEntry {
        name: name.to_string(),
        kind: if is_dir { "dir" } else { "file" }.to_string(),
        size,
        modified: None,
    })
}

/// Extracts the text of the first element whose tag ends with `suffix`
/// inside `block` (e.g. `href`, `getcontentlength`).
fn block_tag_value(block: &str, suffix: &str) -> Option<String> {
    let lowered = block.to_ascii_lowercase();
    let mut index = 0;
    while index < lowered.len() {
        if !lowered[index..].starts_with('<') {
            index += 1;
            continue;
        }
        let name_start = index + 1;
        let mut name_end = name_start;
        while name_end < lowered.len() {
            let byte = lowered.as_bytes()[name_end];
            if byte.is_ascii_alphanumeric() || byte == b':' || byte == b'.' || byte == b'-' || byte == b'_' {
                name_end += 1;
            } else {
                break;
            }
        }
        let tag = &lowered[name_start..name_end];
        if tag.ends_with(suffix) && (tag.len() == suffix.len() || tag.ends_with(&format!(":{}", suffix))) {
            let value_start = lowered[name_end..].find('>')? + name_end + 1;
            let closing = format!("</{tag}>");
            let value_end = lowered[value_start..].find(&closing)? + value_start;
            return Some(block[value_start..value_end].to_string());
        }
        index = name_end;
    }
    None
}

/// True when a tag whose name ends with `suffix` appears in `block`
/// (self-closing or with content).
fn block_tag_contains(block: &str, suffix: &str) -> bool {
    let lowered = block.to_ascii_lowercase();
    let mut index = 0;
    while index < lowered.len() {
        if !lowered[index..].starts_with('<') {
            index += 1;
            continue;
        }
        let name_start = index + 1;
        let mut name_end = name_start;
        while name_end < lowered.len() {
            let byte = lowered.as_bytes()[name_end];
            if byte.is_ascii_alphanumeric() || byte == b':' || byte == b'.' || byte == b'-' || byte == b'_' {
                name_end += 1;
            } else {
                break;
            }
        }
        let tag = &lowered[name_start..name_end];
        if tag.ends_with(suffix) {
            return true;
        }
        index = name_end;
    }
    false
}

/// Percent-decoding for hrefs; invalid escapes are kept verbatim.
fn decode_percent(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = (bytes[index + 1] as char).to_digit(16);
            let low = (bytes[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                out.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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
        "missing command: check | status | sync | upload | watch | ls | download | delete | validate | autodetect",
    )?;
    let (config, project_root) = load_config(args.config_path.clone())?;

    match command.as_str() {
        "validate" => {
            print_validate(&config, args.json)?;
            return Ok(());
        }
        _ => {} // all other commands continue to server selection below
    }

    let command = command.as_str();
    let server = select_server(&config, args.server_name.clone())?;

    match command {
        "status" => print_status(&config, server, &project_root),
        "check" => check_server(server, &project_root, args.dry_run)?,
        "sync" => sync_server(server, &project_root, args.dry_run)?,
        "upload" => {
            if let (Some(path), Some(from)) = (&args.remote_path, args.from.as_ref().or(args.file.as_ref())) {
                let relative = normalize_relative_path(path)?;
                let from_abs = absolute_local_path(from)?;
                upload_file(server, &project_root, &relative, &from_abs, args.dry_run)?
            } else {
                let file = args.file.clone().context("upload requires --file (or --path and --from)")?;
                upload_server(server, &project_root, &file, args.dry_run)?
            }
        }
        "watch" => watch_server(server, &project_root, args.interval)?,
        "autodetect" => run_autodetect(server, args.json)?,
        "ls" => list_server(server, &project_root, args.remote_path.clone(), args.json)?,
        "download" => {
            let path = args
                .remote_path
                .clone()
                .context("download requires --path")?;
            let to = args.to.clone().context("download requires --to")?;
            download_server(server, &project_root, &path, &to, args.json)?
        }
        "delete" => {
            let path = args
                .remote_path
                .clone()
                .context("delete requires --path")?;
            delete_server(
                server,
                &project_root,
                &path,
                args.recursive,
                args.allow_delete,
                args.json,
            )?
        }
        other => bail!("unknown command: {other}"),
    }

    Ok(())
}

fn print_help() {
    println!(
        "php-deploy [--config PATH] [--server NAME] [--dry-run] [--json] [--version] <command>\n\n\
         commands:\n\
           status            show configured servers and resolved paths\n\
           check             validate config and test connectivity\n\
           validate [--json] report config warnings without connecting\n\
           autodetect [--json] probe standard roots ('/', '/var/www', ...)\n\
           sync              upload all non-excluded files\n\
           upload --file P   upload one file\n\
           watch [--interval N]  poll local files and upload changes\n\
           ls [--path P] [--json]  list a remote directory\n\
           download --path P --to LOCAL [--json]  fetch a remote file\n\
           delete --path P [--recursive] --allow-delete [--json]  remove a remote entry\n\n\
         safety: --path must be relative and stay under remote_root ('..' rejected)\n\
          config: .zed/deploy.json (local_root relative to project root)\n\
          protocols: local, in-place, sftp (ssh auth: agent [default], key via\n\
           ssh_key_path, password via ssh_password + the sshpass binary),\n\
           ftp/ftps, webdav/webdavs (curl-based, passive_mode honoured for ftp)"
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

/// Resolves the in-place remote root and verifies it lives inside the project
/// (local_root) by canonicalizing the nearest existing ancestor. Prevents an
/// absolute or `..`-containing `remote_root` from pointing outside the
/// project on any in-place operation.
fn in_place_root(server: &Server, project_root: &Path) -> Result<PathBuf> {
    let base = local_root(server, project_root);
    let canonical_base = fs::canonicalize(&base)
        .with_context(|| format!("in-place local root does not exist: {}", base.display()))?;
    for component in Path::new(&server.remote_root).components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("in-place: path escapes project")
            }
            _ => {}
        }
    }
    let target = base.join(&server.remote_root);
    let mut existing = target.as_path();
    loop {
        if existing.exists() {
            break;
        }
        existing = existing
            .parent()
            .with_context(|| format!("in-place: cannot resolve {}", target.display()))?;
    }
    let canonical_existing = fs::canonicalize(existing)
        .with_context(|| format!("in-place: cannot resolve {}", existing.display()))?;
    if !canonical_existing.starts_with(&canonical_base) {
        bail!("in-place: path escapes project");
    }
    Ok(target)
}

/// Guards the in-place root for whole-tree operations (sync/upload).
fn ensure_in_place(server: &Server, project_root: &Path) -> Result<()> {
    if server.protocol == "in-place" {
        in_place_root(server, project_root)?;
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct ValidateEntry {
    name: String,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ValidateResponse {
    servers: Vec<ValidateEntry>,
}

/// Config-level warnings, as rendered by the `validate` command. The exact
/// warning strings are part of the panel contract; change with care.
fn validate_warnings(server: &Server) -> Vec<String> {
    const KNOWN: [&str; 5] = [
        "local",
        "in-place",
        "sftp",
        "ftp",
        "ftps",
    ];

    if !KNOWN.contains(&server.protocol.as_str())
        && !server.protocol.starts_with("webdav")
    {
        return vec![format!("Unknown protocol {}", server.protocol)];
    }

    let mut warnings = Vec::new();
    match server.protocol.as_str() {
        "sftp" => {
            if server.host.as_deref().unwrap_or("").is_empty() {
                warnings.push("SFTP host is not specified".to_string());
            }
        }
        "local" => {
            if server.local_root.as_deref().unwrap_or("").is_empty() {
                warnings.push("Folder is not specified".to_string());
            }
        }
        _ => {}
    }

    if server.remote_root.is_empty() {
        match server.protocol.as_str() {
            "ftp" | "ftps" => {
                warnings.push("Root path is not set — '/' assumed".to_string())
            }
            _ => warnings.push("Root path is not specified".to_string()),
        }
    }

    warnings
}

fn print_validate(config: &Config, json: bool) -> Result<()> {
    if json {
        let response = ValidateResponse {
            servers: config
                .servers
                .iter()
                .map(|server| ValidateEntry {
                    name: server.name.clone(),
                    warnings: validate_warnings(server),
                })
                .collect(),
        };
        println!("{}", serde_json::to_string(&response)?);
    } else {
        for server in &config.servers {
            for warning in validate_warnings(server) {
                println!("{}: {}", server.name, warning);
            }
        }
    }
    Ok(())
}

/// Candidate roots probed in order by `autodetect`.
const AUTODETECT_ROOTS: [&str; 4] = ["/", "/var/www", "/srv", "/var/www/vhosts"];

/// Per-attempt budget: a probe that has not finished within the limit is
/// killed and treated as a failure.
const AUTODETECT_LIMIT: Duration = Duration::from_secs(2);

/// Runs one root probe against a single candidate. Returns success only when
/// the underlying operation completed cleanly (and within the time budget).
fn probe_root(server: &Server, root: &str) -> bool {
    let mut candidate = server.clone();
    candidate.remote_root = root.to_string();
    let result = match candidate.protocol.as_str() {
        "local" => PathBuf::from(root).is_dir(),
        "in-place" => probe_in_place_root(&candidate),
        "sftp" => probe_sftp_root(&candidate),
        "ftp" | "ftps" => probe_curl(&candidate, ftp_list_command(&candidate).ok()),
        "webdav" | "webdavs" => probe_curl(&candidate, webdav_list_command(&candidate).ok()),
        _ => false,
    };
    result
}

fn probe_in_place_root(server: &Server) -> bool {
    match project_root_for_probe() {
        Some(project_root) => in_place_root(server, &project_root)
            .map(|path| path.is_dir())
            .unwrap_or(false),
        None => false,
    }
}

/// The project root as the CLI would resolve it at runtime; used only by the
/// in-place autodetect probe.
fn project_root_for_probe() -> Option<PathBuf> {
    env::current_dir().ok()
}

fn probe_sftp_root(server: &Server) -> bool {
    let Ok(mut command) = ssh_base_command(server, "ssh") else {
        return false;
    };
    let host = match server.host.as_deref() {
        Some(host) => host,
        None => return false,
    };
    let port = server.port.unwrap_or(22).to_string();
    let destination = match server.user.as_deref() {
        Some(user) => format!("{user}@{host}"),
        None => host.to_string(),
    };
    let remote = build_remote_path(&server.remote_root, ".");
    command.args([
        "-o",
        "ConnectTimeout=5",
        "-p",
        port.as_str(),
        destination.as_str(),
        &format!("ls -1p -- {}", shell_quote(&remote)),
    ]);
    run_command_probe(command, AUTODETECT_LIMIT)
}

fn probe_curl(_server: &Server, command: Option<Command>) -> bool {
    match command {
        Some(command) => run_command_probe(command, AUTODETECT_LIMIT),
        None => false,
    }
}

/// Runs a command for at most `limit`, killing it afterwards. Output is
/// captured (and discarded) so probes never pollute the console.
fn run_command_probe(mut command: Command, limit: Duration) -> bool {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return false,
    };
    let deadline = std::time::Instant::now() + limit;
    loop {
        match child
            .try_wait()
            .map(|status| status.map(|status| status.success()))
        {
            Ok(Some(success)) => return success,
            Ok(None) => {}
            Err(_) => return false,
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

/// Shared autodetect loop: evaluates candidates in order and returns the
/// first root whose probe succeeds.
fn autodetect_roots_with(candidates: &[&str], mut probe: impl FnMut(&str) -> bool) -> Option<String> {
    candidates
        .iter()
        .find(|candidate| probe(candidate))
        .map(|candidate| candidate.to_string())
}

#[derive(Debug, Serialize)]
struct AutodetectResponse {
    root: Option<String>,
}

fn run_autodetect(server: &Server, json: bool) -> Result<()> {
    let root = autodetect_roots_with(&AUTODETECT_ROOTS, |root| probe_root(server, root));
    if json {
        let response = AutodetectResponse { root };
        println!("{}", serde_json::to_string(&response)?);
    } else {
        match root {
            Some(ref root) => println!("root: {root}"),
            None => println!("no root found"),
        }
    }
    Ok(())
}

fn check_server(server: &Server, project_root: &Path, dry_run: bool) -> Result<()> {
    let root = local_root(server, project_root);
    if !root.exists() {
        bail!("local root does not exist: {}", root.display());
    }

    match server.protocol.as_str() {
        "local" | "in-place" => {
            let destination = match server.protocol.as_str() {
                "in-place" => in_place_root(server, project_root)?,
                _ => PathBuf::from(&server.remote_root),
            };
            println!(
                "{} transport: {} -> {} ({})",
                server.protocol,
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
        "webdav" | "webdavs" => {
            let url = webdav_url(server, ".", true)?;
            let mut command = webdav_command(server)?;
            command.args(["-X", "PROPFIND", "-H", "Depth: 1"]);
            command.arg(url.clone());
            if dry_run {
                println!("curl check: {url}");
            } else {
                run_curl(command, server.curl_secret())?;
                println!("{} connectivity ok", server.protocol);
            }
        }
        other => bail!("unsupported protocol: {other}"),
    }

    Ok(())
}

fn list_server(
    server: &Server,
    project_root: &Path,
    path: Option<String>,
    json: bool,
) -> Result<()> {
    let requested = path.unwrap_or_else(|| ".".to_string());
    let mut entries = match server.protocol.as_str() {
        "local" | "in-place" => list_local(server, project_root, &requested)?,
        "sftp" => list_sftp(server, &requested)?,
        "ftp" | "ftps" => list_ftp(server, &requested)?,
        "webdav" | "webdavs" => list_webdav(server, &requested)?,
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

fn list_local(server: &Server, project_root: &Path, path: &str) -> Result<Vec<LsEntry>> {
    // Filesystem transports join the request; rejecting `..`/absolute here
    // keeps listings inside the remote root the same way downloads do.
    let relative = if path == "." {
        PathBuf::new()
    } else {
        normalize_relative_path(path)?
    };
    let directory = match server.protocol.as_str() {
        "in-place" => in_place_root(server, project_root)?.join(relative),
        _ => PathBuf::from(&server.remote_root).join(relative),
    };
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
    let rel_str = relative.to_string_lossy().replace('\\', "/");
    let rel_str = rel_str.trim_start_matches('/');
    let root = server.remote_root.trim_end_matches('/');
    if root.is_empty() || root == "/" {
        format!("/{rel_str}")
    } else {
        let root_trimmed = root.trim_start_matches('/');
        if rel_str == root_trimmed || rel_str.starts_with(&format!("{root_trimmed}/")) {
            format!("/{rel_str}")
        } else {
            format!("{root}/{rel_str}")
        }
    }
}

fn download_server(
    server: &Server,
    project_root: &Path,
    path: &str,
    to: &Path,
    json: bool,
) -> Result<()> {
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
        "local" | "in-place" => {
            let base = match server.protocol.as_str() {
                "in-place" => in_place_root(server, project_root)?,
                _ => PathBuf::from(&server.remote_root),
            };
            let source = resolve_local_remote_path(
                &base.to_string_lossy(),
                &relative,
            )?;
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
        "ftp" | "ftps" => download_ftp(server, &relative, &target)?,
        "webdav" | "webdavs" => download_webdav(server, &relative, &target)?,
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
    project_root: &Path,
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
        "local" | "in-place" => {
            let base = match server.protocol.as_str() {
                "in-place" => in_place_root(server, project_root)?,
                _ => PathBuf::from(&server.remote_root),
            };
            let target = resolve_local_remote_path(
                &base.to_string_lossy(),
                &relative,
            )?;
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
            let remote = sftp_remote_path(server, &relative);
            if recursive {
                let _ = run_ssh_capture(
                    server,
                    &format!("rm -rf {}", shell_quote(&remote)),
                )?;
            } else {
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
        }
        "ftp" | "ftps" => {
            if recursive {
                bail!("recursive delete is not supported over ftp; delete files individually");
            }
            delete_ftp(server, &relative)?;
        }
        "webdav" | "webdavs" => delete_webdav(server, &relative)?,
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
    ensure_in_place(server, project_root)?;
    if server.use_rsync && server.protocol == "sftp" && server.resolved_auth() != SshAuth::Password
    {
        return sync_rsync(server, &root, dry_run);
    }
    let files = collect_files(&root, &server.exclude)?;
    println!("{} files to deploy", files.len());

    match server.protocol.as_str() {
        "local" | "in-place" => {
            let destination = match server.protocol.as_str() {
                "in-place" => in_place_root(server, project_root)?,
                _ => PathBuf::from(&server.remote_root),
            };
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
        "ftp" | "ftps" | "webdav" | "webdavs" => {
            for file in files {
                let relative = file.strip_prefix(&root)?;
                upload_file(server, project_root, relative, &file, dry_run)?;
            }
        }
        other => bail!("unsupported protocol: {other}"),
    }

    Ok(())
}

/// The `--rsh` program string: a BatchMode ssh, with a key when one is
/// configured. Password authentication cannot use BatchMode, so it never
/// reaches rsync (the caller falls back to the sftp batch).
fn rsync_rsh(server: &Server) -> String {
    let port = server.port.unwrap_or(22);
    let mut rsh = format!("ssh -p {port} -o BatchMode=yes");
    if let Some(key) = server
        .ssh_key_path
        .as_deref()
        .filter(|key| !key.is_empty())
    {
        rsh.push_str(&format!(" -i {key} -o IdentitiesOnly=yes"));
    }
    rsh
}

/// Common rsync arguments: `-az [--delete] --rsh "ssh …"` plus one
/// `--exclude=<pattern>` per configured exclude. If custom `rsync_args`
/// are configured, they override the `-az [--delete]` prefix.
fn rsync_args(server: &Server) -> Vec<String> {
    let mut args = if let Some(custom) = &server.rsync_args {
        custom
            .split_whitespace()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    } else {
        let mut default_args = vec!["-az".to_string()];
        if server.rsync_delete.unwrap_or(true) {
            default_args.push("--delete".to_string());
        }
        default_args
    };
    args.push("--rsh".to_string());
    args.push(rsync_rsh(server));
    for pattern in &server.exclude {
        args.push(format!("--exclude={pattern}"));
    }
    args
}

fn rsync_binary(server: &Server) -> &str {
    server.rsync_path.as_deref().unwrap_or("rsync")
}

fn rsync_destination(server: &Server) -> String {
    let host = server.host.as_deref().unwrap_or("").to_string();
    let destination = match server.user.as_deref() {
        Some(user) if !user.is_empty() => format!("{user}@{host}"),
        _ => host,
    };
    format!("{destination}:{}", trim_root(&server.remote_root))
}

fn trim_root(root: &str) -> String {
    let trimmed = root.trim_matches('/');
    if root.starts_with('/') {
        format!("/{trimmed}/")
    } else {
        format!("{trimmed}/")
    }
}

/// rsync push for sync over sftp: copies `local_root/` to
/// `user@host:remote_root/` in one shot. Prints the trailing `via rsync` info
/// line the panel looks for.
fn sync_rsync(server: &Server, root: &Path, dry_run: bool) -> Result<()> {
    let mut args = rsync_args(server);
    if dry_run {
        args.push("-n".to_string());
    }
    args.push(format!("{}", root.display()));
    args.push(rsync_destination(server));
    if dry_run {
        println!("rsync {}", args.join(" "));
    } else {
        let output = Command::new(rsync_binary(server))
            .args(&args)
            .output()
            .with_context(|| format!("failed to run rsync (use_rsync with {})", server.protocol))?;
        if !output.status.success() {
            bail!(
                "rsync failed: {}",
                scrub(
                    &String::from_utf8_lossy(&output.stderr).trim(),
                    server.ssh_secret()
                )
            );
        }
    }
    println!("via rsync");
    Ok(())
}

/// rsync push for a single file upload.
fn upload_rsync_file(
    server: &Server,
    relative: &Path,
    absolute: &Path,
    dry_run: bool,
) -> Result<()> {
    let mut args = rsync_args(server);
    args.retain(|arg| arg != "--delete"); // --delete is meaningless for a single file
    if dry_run {
        args.push("-n".to_string());
    }
    args.push(absolute.to_string_lossy().into_owned());
    args.push(format!(
        "{}{}",
        rsync_destination(server),
        relative.to_string_lossy().replace('\\', "/")
    ));
    if dry_run {
        println!("rsync {}", args.join(" "));
    } else {
        let output = Command::new(rsync_binary(server))
            .args(&args)
            .output()
            .context("failed to run rsync")?;
        if !output.status.success() {
            bail!(
                "rsync failed: {}",
                scrub(
                    &String::from_utf8_lossy(&output.stderr).trim(),
                    server.ssh_secret()
                )
            );
        }
    }
    println!("via rsync");
    Ok(())
}

fn upload_server(
    server: &Server,
    project_root: &Path,
    file: &Path,
    dry_run: bool,
) -> Result<()> {
    ensure_in_place(server, project_root)?;
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

    if server.use_rsync && server.protocol == "sftp" && server.resolved_auth() != SshAuth::Password
    {
        return upload_rsync_file(server, relative, &absolute, dry_run);
    }

    upload_file(server, project_root, relative, &absolute, dry_run)
}

fn upload_file(
    server: &Server,
    project_root: &Path,
    relative: &Path,
    absolute: &Path,
    dry_run: bool,
) -> Result<()> {
    match server.protocol.as_str() {
        "local" | "in-place" => {
            let target = match server.protocol.as_str() {
                "in-place" => in_place_root(server, project_root)?.join(relative),
                _ => PathBuf::from(&server.remote_root).join(relative),
            };
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
        "ftp" | "ftps" => {
            if dry_run {
                println!(
                    "upload {} -> {}",
                    absolute.display(),
                    ftp_file_url(
                        server,
                        &relative.to_string_lossy().replace('\\', "/")
                    )
                    .unwrap_or_default()
                );
                return Ok(());
            }
            upload_ftp(server, relative, absolute)?;
            println!("uploaded {}", relative.display());
        }
        "webdav" | "webdavs" => {
            if dry_run {
                println!(
                    "upload {} -> {}",
                    absolute.display(),
                    webdav_url(
                        server,
                        &relative.to_string_lossy().replace('\\', "/"),
                        false
                    )
                    .unwrap_or_default()
                );
                return Ok(());
            }
            upload_webdav(server, relative, absolute)?;
            println!("uploaded {}", relative.display());
        }
        other => bail!("unsupported protocol: {other}"),
    }
    Ok(())
}

fn append_sftp_put(batch: &mut String, server: &Server, relative: &Path, absolute: &Path) {
    if let Some(parent) = relative.parent() {
        let mut directory = PathBuf::new();
        for component in parent.components() {
            directory.push(component);
            let dir_path = sftp_remote_path(server, &directory);
            batch.push_str(&format!("-mkdir \"{dir_path}\"\n"));
        }
    }
    let remote_file = sftp_remote_path(server, relative);
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
            if let Err(error) = upload_file(server, project_root, relative, path, false) {
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
    fn sftp_remote_path_deduplicates_root_prefix() {
        let mut s = server(r#"{"name":"srv","protocol":"sftp","remote_root":"/home"}"#);

        // When relative path already starts with /home (e.g. from SFTP tree traversal)
        let rel_with_home = Path::new("home/gitlab-runner/builds/app.json");
        assert_eq!(
            sftp_remote_path(&s, rel_with_home),
            "/home/gitlab-runner/builds/app.json"
        );

        // When relative path does not start with /home (pure relative)
        let rel_clean = Path::new("gitlab-runner/builds/app.json");
        assert_eq!(
            sftp_remote_path(&s, rel_clean),
            "/home/gitlab-runner/builds/app.json"
        );

        // When remote_root is /
        s.remote_root = "/".to_string();
        assert_eq!(
            sftp_remote_path(&s, rel_with_home),
            "/home/gitlab-runner/builds/app.json"
        );
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

    // -------------------------------------------------------- ftp/ftps

    #[test]
    fn ftp_list_parses_unix_lines() {
        let listing = concat!(
            "total 20\n",
            "drwxr-xr-x   2 deploy deploy  4096 Mar 13 2024 subdir\n",
            "-rw-r--r--   1 deploy deploy    66 Mar 13 2024 index.php\n",
            "-rw-r--r--   1 deploy deploy    12 Mar 13 2024 .hidden\n",
            "lrwxrwxrwx   1 deploy deploy    13 Mar 13 2024 link -> /pub/stuff/\n",
            "-rw-r--r--   1 deploy deploy   512 Mar 13 2024 file with spaces.php\n",
            "drwxr-xr-x   2 deploy deploy  4096 Oct 27 10:33 recent\n"
        );
        let entries = parse_ftp_list(listing);
        let names: Vec<(&str, &str)> = entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.kind.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("subdir", "dir"),
                ("index.php", "file"),
                (".hidden", "file"),
                ("link", "dir"),
                ("file with spaces.php", "file"),
                ("recent", "dir"),
            ]
        );
        let index = entries.iter().find(|e| e.name == "index.php").unwrap();
        assert_eq!(index.size, Some(66));
        assert_eq!(index.modified, Some(1710288000));
        let recent = entries.iter().find(|e| e.name == "recent").unwrap();
        assert!(recent.modified.is_some(), "recent form must yield a timestamp");
        let now = now_unix_seconds() as u64;
        assert!(recent.modified.unwrap() <= now + 86_400);
        assert!(recent.modified.unwrap() >= now - 367 * 86_400);
    }

    #[test]
    fn ftp_list_skips_junk_lines() {
        let entries = parse_ftp_list("total 8\n\ngarbage\ndrwx too short\n");
        assert!(entries.is_empty());
    }

    #[test]
    fn ftp_date_absolutely_uses_year_form() {
        assert_eq!(parse_ftp_date(&["Mar", " 13", "2024"]), Some(1710288000));
        assert_eq!(parse_ftp_date(&["Jan", " 1", "1970"]), Some(0));
        assert_eq!(parse_ftp_date(&["Bad", " 1", "2024"]), None);
    }

    #[test]
    fn ftp_date_recent_form_is_inside_a_year_window() {
        let modified = parse_ftp_date(&["Oct", "27", "10:33"]).unwrap();
        let now = now_unix_seconds() as u64;
        assert!(modified <= now + 86_400);
        assert!(modified >= now - 367 * 86_400);
    }

    #[test]
    fn ftp_list_url_forms_root_path_and_port() {
        let s = server(r#"{"name":"f","protocol":"ftp","host":"h","remote_root":"/pub"}"#);
        assert_eq!(ftp_list_url(&s, ".").unwrap(), "ftp://h/pub/");
        assert_eq!(ftp_list_url(&s, "sub/x").unwrap(), "ftp://h/pub/sub/x/");

        let s = server(
            r#"{"name":"f","protocol":"ftps","host":"127.0.0.1","port":2121,"remote_root":"/pub"}"#,
        );
        assert_eq!(ftp_list_url(&s, ".").unwrap(), "ftps://127.0.0.1:2121/pub/");

        let s = server(r#"{"name":"f","protocol":"ftp","host":"h"}"#);
        assert_eq!(ftp_list_url(&s, ".").unwrap(), "ftp://h/");
    }

    #[test]
    fn ftp_auth_flags_per_user_shape() {
        let s = server(
            r#"{"name":"f","protocol":"ftp","host":"h","remote_root":"/pub",
                "user":"deploy","password":"pw"}"#,
        );
        let mut command = ftp_list_command(&s).unwrap();
        command.arg("ftp://h/");
        let (_, args) = argv(&command);
        assert_eq!(
            args,
            vec![
                "-sS",
                "--connect-timeout",
                "5",
                "--max-time",
                "30",
                "-u",
                "deploy:pw",
                "ftp://h/"
            ]
        );

        // anonymoususer without a password still carries -u with empty secret.
        let s = server(
            r#"{"name":"f","protocol":"ftp","host":"h","remote_root":"/pub","user":"anonymous"}"#,
        );
        let mut command = ftp_list_command(&s).unwrap();
        let (_, args) = argv(&command);
        assert_eq!(args.last().unwrap(), "anonymous:");

        // No user: no -u at all.
        let s = server(r#"{"name":"f","protocol":"ftp","host":"h","remote_root":"/pub"}"#);
        let (_, args) = argv(&ftp_list_command(&s).unwrap());
        assert!(!args.contains(&"-u".to_string()));
    }

    #[test]
    fn ftps_ops_always_request_tls() {
        let s = server(r#"{"name":"f","protocol":"ftps","host":"h","remote_root":"/pub"}"#);
        let (_, args) = argv(&curl_transport_command(&s).unwrap());
        assert_eq!(args, vec!["--ssl-reqd"]);
    }

    #[test]
    fn ftp_active_mode_carries_dash_p_into_ops() {
        let s = server(
            r#"{"name":"f","protocol":"ftp","host":"h","remote_root":"/pub","passive_mode":false}"#,
        );
        let (_, args) = argv(&curl_transport_command(&s).unwrap());
        assert_eq!(args.first().unwrap(), "-P");
        assert_eq!(args[1], "-");
    }

    #[test]
    fn ftp_upload_args_carry_dash_t_and_create_dirs() {
        let s = server(r#"{"name":"f","protocol":"ftp","host":"h","remote_root":"/pub"}"#);
        let mut command = with_args(
            curl_transport_command(&s).unwrap(),
            &["-sS", "--connect-timeout", "5", "--max-time", "300", "--ftp-create-dirs"],
        );
        command.args(["-T", "/tmp/src.php"].as_slice());
        command.arg("ftp://h/pub/src.php");
        let (_, args) = argv(&command);
        assert!(args.iter().any(|arg| arg == "--ftp-create-dirs"));
        assert_eq!(args.iter().filter(|arg| arg.as_str() == "-T").count(), 1);
        assert!(args.contains(&"ftp://h/pub/src.php".to_string()));
    }

    // ------------------------------------------------------- webdav

    const PROPFIND_DAV_NS: &str = concat!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n",
        "<D:multistatus xmlns:D=\"DAV:\">\n",
        "  <D:response><D:href>/dav/</D:href>",
        "<D:propstat><D:prop><D:resourcetype><D:collection/>",
        "</D:resourcetype></D:prop></D:propstat></D:response>\n",
        "  <D:response><D:href>/dav/my%20file.txt</D:href>",
        "<D:propstat><D:prop><D:getcontentlength>66</D:getcontentlength>",
        "<D:resourcetype/></D:prop></D:propstat></D:response>\n",
        "  <D:response><D:href>/dav/subdir/</D:href>",
        "<D:propstat><D:prop><D:resourcetype><D:collection/>",
        "</D:resourcetype></D:prop></D:propstat></D:response>\n",
        "</D:multistatus>\n"
    );

    #[test]
    fn propfind_dav_namespace_blocks_parse() {
        let entries = parse_dav_listing(PROPFIND_DAV_NS, "/dav");
        let names: Vec<(&str, &str)> = entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.kind.as_str()))
            .collect();
        assert_eq!(names, vec![("my file.txt", "file"), ("subdir", "dir")]);
        let size = entries.iter().find(|e| e.name == "my file.txt").unwrap().size;
        assert_eq!(size, Some(66));
    }

    #[test]
    fn propfind_unprefixed_elements_parse() {
        let xml = concat!(
            "<multistatus xmlns=\"DAV:\">\n",
            "  <response><href>/x/</href>",
            "<propstat><prop><resourcetype><collection/></resourcetype></prop></propstat></response>\n",
            "  <response><href>/x/b.php</href>",
            "<propstat><prop><getcontentlength>5</getcontentlength></prop></propstat></response>\n",
            "</multistatus>\n"
        );
        let entries = parse_dav_listing(xml, "/x");
        let names: Vec<(&str, &str)> = entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.kind.as_str()))
            .collect();
        assert_eq!(names, vec![("b.php", "file")]);
        assert_eq!(entries[0].size, Some(5));
    }

    #[test]
    fn propfind_skips_self_and_guard_urls_decode() {
        // Self block (href == request path) is missing from the result, and
        // `my%20file.txt` becomes `my file.txt`.
        let entries = parse_dav_listing(PROPFIND_DAV_NS, "/dav");
        assert!(!entries.iter().any(|entry| entry.name == "dav"));
    }

    #[test]
    fn webdav_ls_arguments_carry_propfind_depth() {
        let s = server(
            r#"{"name":"w","protocol":"webdav","host":"h","remote_root":"/dav",
                "user":"u","password":"pw"}"#,
        );
        let mut command = webdav_list_command(&s).unwrap();
        command.arg("http://h/dav/");
        let (_, args) = argv(&command);
        assert_eq!(
            args,
            vec![
                "-sS",
                "--connect-timeout",
                "5",
                "--max-time",
                "30",
                "-f",
                "-u",
                "u:pw",
                "-X",
                "PROPFIND",
                "-H",
                "Depth: 1",
                "http://h/dav/"
            ]
        );
    }

    #[test]
    fn webdav_url_scheme_matches_protocol() {
        let s = server(r#"{"name":"w","protocol":"webdav","host":"w.example","port":8443,"remote_root":"/dav"}"#);
        assert_eq!(
            webdav_url(&s, "sub/leaf.txt", false).unwrap(),
            "http://w.example:8443/dav/sub/leaf.txt"
        );

        let s = server(r#"{"name":"w","protocol":"webdavs","host":"w.example","remote_root":"/dav"}"#);
        assert_eq!(webdav_url(&s, ".", true).unwrap(), "https://w.example/dav/");

        let s = server(r#"{"name":"w","protocol":"http","remote_root":"/dav"}"#);
        assert!(webdav_url(&s, ".", false).is_err());
    }

    #[test]
    fn webdav_parent_dirs_are_shallowest_first() {
        assert_eq!(webdav_parent_dirs("a/b/c.php"), vec!["a", "a/b"]);
        assert!(webdav_parent_dirs("c.php").is_empty());
    }

    #[test]
    fn curl_secret_falls_back_to_ssh_password_and_scrubs() {
        let json = r#"{"name":"w","protocol":"webdav","host":"h","remote_root":"/dav",
            "ssh_password":"hidden-ssh"}"#;
        assert_eq!(server(json).curl_secret(), Some("hidden-ssh"));

        let json = r#"{"name":"w","protocol":"webdav","host":"h","remote_root":"/dav",
            "password":"curl-pw"}"#;
        assert_eq!(server(json).curl_secret(), Some("curl-pw"));

        assert_eq!(
            scrub("curl failed: bad auth with curl-pw", server(json).curl_secret()),
            "curl failed: bad auth with ***"
        );
        assert_eq!(server(r#"{"name":"w","protocol":"webdav","remote_root":"/dav"}"#).curl_secret(), None);
    }

    // ------------------------------------------------------ validate

    #[test]
    fn validate_sftp_without_host_warns() {
        let s = server(r#"{"name":"sftp","protocol":"sftp","remote_root":"/r"}"#);
        assert_eq!(validate_warnings(&s), vec!["SFTP host is not specified"]);
    }

    #[test]
    fn validate_local_without_folder_warns() {
        let s = server(r#"{"name":"l","protocol":"local","remote_root":"/t"}"#);
        assert_eq!(validate_warnings(&s), vec!["Folder is not specified"]);
    }

    #[test]
    fn validate_missing_root_warns_for_each_type() {
        let sftp = server(r#"{"name":"s","protocol":"sftp","host":"h"}"#);
        let in_place =
            server(r#"{"name":"i","protocol":"in-place","local_root":"."}"#);
        let webdav = server(r#"{"name":"w","protocol":"webdav","host":"h"}"#);
        for s in [&sftp, &in_place, &webdav] {
            assert_eq!(validate_warnings(s), vec!["Root path is not specified"]);
        }
    }

    #[test]
    fn validate_ftp_root_message_assumes_slash() {
        let s = server(r#"{"name":"f","protocol":"ftp","host":"h"}"#);
        assert_eq!(
            validate_warnings(&s),
            vec!["Root path is not set — '/' assumed"]
        );
        let s = server(r#"{"name":"f","protocol":"ftps","host":"h"}"#);
        assert_eq!(
            validate_warnings(&s),
            vec!["Root path is not set — '/' assumed"]
        );
    }

    #[test]
    fn validate_unknown_protocol_reads_verbatim() {
        let s = server(r#"{"name":"u","protocol":"s3"}"#);
        assert_eq!(validate_warnings(&s), vec!["Unknown protocol s3"]);
    }

    #[test]
    fn validate_json_output_lists_every_server() {
        let module: Config = serde_json::from_str(
            r#"{"servers":[
                {"name":"a","protocol":"sftp","host":"h","remote_root":"/r"},
                {"name":"b","protocol":"ftp","host":"h"}
            ]}"#,
        )
        .unwrap();
        let response = ValidateResponse {
            servers: module
                .servers
                .iter()
                .map(|s| ValidateEntry {
                    name: s.name.clone(),
                    warnings: validate_warnings(s),
                })
                .collect(),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(
            json,
            r#"{"servers":[{"name":"a","warnings":[]},{"name":"b","warnings":["Root path is not set — '/' assumed"]}]}"#
        );
    }

    // ---------------------------------------------------- autodetect

    #[test]
    fn autodetect_candidate_order_is_fixed() {
        assert_eq!(
            AUTODETECT_ROOTS,
            ["/", "/var/www", "/srv", "/var/www/vhosts"]
        );
    }

    #[test]
    fn autodetect_stops_at_first_success_in_order() {
        let mut seen: Vec<String> = Vec::new();
        let root = autodetect_roots_with(&AUTODETECT_ROOTS, |candidate| {
            seen.push(candidate.to_string());
            candidate == "/srv"
        });
        assert_eq!(root.as_deref(), Some("/srv"));
        assert_eq!(seen, vec!["/", "/var/www", "/srv"]);

        let root = autodetect_roots_with(&AUTODETECT_ROOTS, |_| false);
        assert_eq!(root, None);
    }

    // -------------------------------------------------- in-place guard

    fn scratch(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("php-deploy-it-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn in_place_resolves_inside_local_root() {
        let project = scratch("inside");
        fs::create_dir_all(project.join("webroot")).unwrap();
        let s = server(&format!(
            r#"{{"name":"i","protocol":"in-place","remote_root":"webroot"}}"#
        ));
        assert_eq!(
            in_place_root(&s, &project).unwrap(),
            project.join("webroot")
        );
        let _ = fs::remove_dir_all(&project);
    }

    #[test]
    fn in_place_rejects_escape_from_project() {
        let project = scratch("escape");
        let outside = project.parent().unwrap().join("php-deploy-outside");
        fs::create_dir_all(&outside).unwrap();

        // A `..`-bearing root is rejected outright by the traversal guard,
        // even when the target directory itself exists.
        let parent = Server {
            remote_root: "..".to_string(),
            ..server(r#"{"name":"i","protocol":"in-place","local_root":"."}"#)
        };
        let error = in_place_root(&parent, &project).unwrap_err().to_string();
        assert_eq!(error, "in-place: path escapes project");

        let nested = Server {
            remote_root: "webroot/../../elsewhere".to_string(),
            ..server(r#"{"name":"i","protocol":"in-place","local_root":"."}"#)
        };
        let error = in_place_root(&nested, &project).unwrap_err().to_string();
        assert_eq!(error, "in-place: path escapes project");

        // An absolute remote root must also fail the canonical check.
        let absolute = Server {
            remote_root: outside
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            ..server(r#"{"name":"i","protocol":"in-place","local_root":"."}"#)
        };
        let error = in_place_root(&absolute, &project).unwrap_err().to_string();
        assert_eq!(error, "in-place: path escapes project");

        let _ = fs::remove_dir_all(&project);
        let _ = fs::remove_dir_all(&outside);
    }

    // ---------------------------------------------------- rsync push

    #[test]
    fn rsync_args_carry_rsh_and_excludes() {
        let s = server(
            r#"{"name":"w","protocol":"sftp","host":"web1","port":2222,"user":"deploy",
                "remote_root":"webroot","exclude":["vendor/","*.log"],
                "ssh_key_path":"/homes/.ssh/id"}"#,
        );
        assert_eq!(
            rsync_args(&s),
            vec![
                "-az".to_string(),
                "--delete".to_string(),
                "--rsh".to_string(),
                "ssh -p 2222 -o BatchMode=yes -i /homes/.ssh/id -o IdentitiesOnly=yes".to_string(),
                "--exclude=vendor/".to_string(),
                "--exclude=*.log".to_string(),
            ]
        );
        assert_eq!(
            rsync_destination(&s),
            "deploy@web1:webroot/".to_string()
        );
    }

    #[test]
    fn rsync_custom_args_and_binary() {
        let s = server(
            r#"{"name":"w","protocol":"sftp","host":"web1","port":2222,"user":"deploy",
                "remote_root":"webroot","exclude":[],"rsync_path":"/usr/local/bin/rsync",
                "rsync_args":"-avz --checksum","rsync_delete":false}"#,
        );
        assert_eq!(rsync_binary(&s), "/usr/local/bin/rsync");
        assert_eq!(
            rsync_args(&s),
            vec![
                "-avz".to_string(),
                "--checksum".to_string(),
                "--rsh".to_string(),
                "ssh -p 2222 -o BatchMode=yes".to_string(),
            ]
        );
    }

    #[test]
    fn rsync_rsh_defaults_to_agent_mode_ssh() {
        let s = server(r#"{"name":"w","protocol":"sftp","host":"web1"}"#);
        assert_eq!(rsync_rsh(&s), "ssh -p 22 -o BatchMode=yes");
    }

    #[test]
    fn rsync_destination_is_understandable_with_default_port() {
        let s = server(
            r#"{"name":"w","protocol":"sftp","host":"web1","remote_root":"webroot"}"#,
        );
        assert_eq!(rsync_destination(&s), "web1:webroot/");

        let s = server(
            r#"{"name":"w","protocol":"sftp","host":"web1","user":"deploy","remote_root":"/var/www/app"}"#,
        );
        assert_eq!(rsync_destination(&s), "deploy@web1:/var/www/app/");
    }

    // ------------------------------------------------------ e2e flows

    #[test]
    fn in_place_ops_roundtrip_through_public_functions() {
        let project = scratch("inplace-ops");
        fs::write(project.join("a.php"), b"<?php echo 1;\n").unwrap();
        fs::create_dir_all(project.join("webroot")).unwrap();
        fs::write(
            project.join("webroot/old.php"),
            b"<?php // old\n",
        )
        .unwrap();
        let s = server(
            r#"{"name":"i","protocol":"in-place","local_root":".","remote_root":"webroot"}"#,
        );

        // ls inside webroot
        let entries = list_local(&s, &project, ".").unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["old.php"]);

        // download into a temp target
        let target = project.join("downloaded-old.php");
        download_server(&s, &project, "old.php", &target, false).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"<?php // old\n");

        // upload a file from the project into webroot
        upload_file(&s, &project, Path::new("a.php"), &project.join("a.php"), false).unwrap();
        assert!(project.join("webroot/a.php").exists());

        // delete
        delete_server(&s, &project, "a.php", false, true, false).unwrap();
        assert!(!project.join("webroot/a.php").exists());

        let _ = fs::remove_dir_all(&project);
    }

    // --------------------------------------- webdav e2e (mock server)

    fn spawn_mock_dav(root: &'static str) -> (u16, PathBuf) {
        use std::net::{TcpListener, TcpStream};

        let dir = scratch("dav-mock");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker_dir = dir.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let inner_dir = worker_dir.clone();
                        thread::spawn(move || {
                            handle_mock_dav(stream, &inner_dir, root).ok();
                        })
                    }
                    Err(_) => break,
                };
            }
        });

        // Wake the worker so the test never races the bind.
        let _ = TcpStream::connect(("127.0.0.1", port));
        (port, dir)
    }

    fn handle_mock_dav(
        stream: std::net::TcpStream,
        dir: &Path,
        root: &'static str,
    ) -> std::io::Result<()> {
        use std::io::{BufRead, BufReader, Read, Write};

        let mut stream = stream;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let mut parts = line.trim().split_ascii_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let target = parts.next().unwrap_or("/").to_string();

        let mut content_length = 0usize;
        let mut expect_continue = false;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header)?;
            let header = header.trim();
            if header.is_empty() {
                break;
            }
            let lowered = header.to_ascii_lowercase();
            if let Some(rest) = lowered.strip_prefix("content-length:") {
                content_length = rest.trim().parse().unwrap_or(0);
            }
            if lowered.starts_with("expect:") && lowered.contains("100-continue") {
                expect_continue = true;
            }
        }

        let base = format!("/{}", root.trim_matches('/'));
        let rel = target
            .split('?')
            .next()
            .unwrap_or("")
            .strip_prefix(&base)
            .unwrap_or("")
            .trim_start_matches('/')
            .to_string();
        let rel = decode_percent(&rel);

        let write_response = |stream: &mut std::net::TcpStream,
                              code: u32,
                              reason: &str,
                              body: &[u8]|
         -> std::io::Result<()> {
            let head = format!(
                "HTTP/1.1 {code} {reason}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes())?;
            stream.write_all(body)?;
            stream.flush()
        };

        match method.as_str() {
            "PROPFIND" => {
                let mut xml = String::from(
                    "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
                     <D:multistatus xmlns:D=\"DAV:\">\n",
                );
                xml.push_str(&format!(
                    "  <D:response><D:href>{base}/{}</D:href>\
                      <D:propstat><D:prop><D:resourcetype><D:collection/>\
                      </D:resourcetype></D:prop></D:propstat></D:response>\n",
                    rel.is_empty().then(|| "").unwrap_or(&format!("{}/", rel))
                ));
                let mut children: Vec<(String, u64, bool)> = Vec::new();
                for entry in fs::read_dir(dir.join(&rel)) {
                    for entry in entry.flatten() {
                        children.push((
                            entry.file_name().to_string_lossy().into_owned(),
                            entry.metadata().map(|metadata| metadata.len()).unwrap_or(0),
                            entry.metadata().map(|metadata| metadata.is_dir()).unwrap_or(false),
                        ));
                    }
                }
                children.sort();
                for (name, size, is_dir) in children {
                    let tail = if is_dir { "/" } else { "" };
                    let collection = if is_dir {
                        "<D:resourcetype><D:collection/></D:resourcetype>"
                    } else {
                        "<D:resourcetype/>"
                    };
                    xml.push_str(&format!(
                        "  <D:response><D:href>{base}/{}/{}{}</D:href>\
                          <D:propstat><D:prop><D:getcontentlength>{}</D:getcontentlength>{}</D:prop></D:propstat></D:response>\n",
                        rel, name, tail, size, collection
                    ));
                }
                xml.push_str("</D:multistatus>\n");
                write_response(&mut stream, 207, "Multi-Status", xml.as_bytes())
            }
            "PUT" => {
                if expect_continue {
                    stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
                    stream.flush()?;
                }
                if content_length > 0 {
                    let mut body = vec![0u8; content_length];
                    reader.read_exact(&mut body)?;
                    let path = dir.join(&rel);
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(path, &body)?;
                }
                write_response(&mut stream, 201, "Created", b"")
            }
            "GET" => match fs::read(dir.join(&rel)) {
                Ok(bytes) => write_response(&mut stream, 200, "OK", &bytes),
                Err(_) => write_response(&mut stream, 404, "Not Found", b"not found"),
            },
            "MKCOL" => {
                fs::create_dir_all(dir.join(&rel))?;
                write_response(&mut stream, 201, "Created", b"")
            }
            "DELETE" => {
                let path = dir.join(&rel);
                match fs::symlink_metadata(&path) {
                    Ok(metadata) if metadata.is_dir() => {
                        fs::remove_dir_all(&path)?;
                        write_response(&mut stream, 204, "No Content", b"")
                    }
                    Ok(_) => {
                        fs::remove_file(&path)?;
                        write_response(&mut stream, 204, "No Content", b"")
                    }
                    Err(_) => write_response(&mut stream, 404, "Not Found", b"not found"),
                }
            }
            _ => write_response(&mut stream, 405, "Method Not Allowed", b""),
        }
    }

    #[test]
    fn webdav_e2e_real_curl_roundtrip() {
        let (port, dir) = spawn_mock_dav("dav");
        let s = server(
            &format!(
                r#"{{"name":"w","protocol":"webdav","host":"127.0.0.1","port":{port},"remote_root":"/dav"}}"#
            ),
        );

        // ls: the store holds a file and a directory.
        fs::write(dir.join("a.txt"), b"hello").unwrap();
        fs::create_dir_all(dir.join("pkg")).unwrap();
        let entries = list_webdav(&s, ".").unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a.txt", "pkg"]);
        let size = entries.iter().find(|e| e.name == "a.txt").unwrap().size;
        assert_eq!(size, Some(5));
        assert_eq!(
            entries.iter().find(|e| e.name == "pkg").unwrap().kind,
            "dir"
        );

        // upload a nested file (parents are created via MKCOL)
        let body = b"<?php echo 2;\n";
        fs::write(dir.join("plain-source.php"), body).unwrap();
        upload_webdav(&s, Path::new("nested/new.php"), &dir.join("plain-source.php")).unwrap();
        assert_eq!(fs::read(dir.join("nested/new.php")).unwrap(), body);

        // download echoes the exact bytes
        let target = dir.join("downloaded-copy.php");
        download_webdav(&s, Path::new("a.txt"), &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"hello");

        // delete removes the file for real
        delete_webdav(&s, Path::new("a.txt")).unwrap();
        assert!(!dir.join("a.txt").exists());
    }
}
