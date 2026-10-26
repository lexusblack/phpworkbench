use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
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

#[derive(Debug, Deserialize, Clone)]
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
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(2);
    }
}

fn run() -> Result<()> {
    let mut config_path = None;
    let mut server_name = None;
    let mut dry_run = false;
    let mut interval = 2u64;
    let mut file = None;
    let mut command = None;
    let mut remote_path = None;
    let mut json = false;

    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--config" => config_path = Some(PathBuf::from(args.next().context("--config path")?)),
            "--server" => server_name = Some(args.next().context("--server name")?),
            "--file" => file = Some(PathBuf::from(args.next().context("--file path")?)),
            "--path" => remote_path = Some(args.next().context("--path REMOTE_PATH")?),
            "--json" => json = true,
            "--interval" => {
                interval = args
                    .next()
                    .context("--interval seconds")?
                    .parse()
                    .context("invalid --interval")?
            }
            "--dry-run" => dry_run = true,
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            other if !other.starts_with('-') => {
                if command.is_some() {
                    bail!("unexpected argument: {other}");
                }
                command = Some(other.to_string());
            }
            other => bail!("unknown argument: {other}"),
        }
    }

    let command =
        command.context("missing command: check | status | sync | upload | watch | ls")?;
    let (config, project_root) = load_config(config_path)?;
    let server = select_server(&config, server_name)?;

    match command.as_str() {
        "status" => print_status(&config, &server, &project_root),
        "check" => check_server(&server, &project_root, dry_run)?,
        "sync" => sync_server(&server, &project_root, dry_run)?,
        "upload" => {
            let file = file.context("upload requires --file")?;
            upload_server(&server, &project_root, &file, dry_run)?
        }
        "watch" => watch_server(&server, &project_root, interval)?,
        "ls" => list_server(&server, remote_path, json)?,
        other => bail!("unknown command: {other}"),
    }

    Ok(())
}

fn print_help() {
    println!(
        "php-deploy [--config PATH] [--server NAME] [--dry-run] <command>\n\n\
         commands:\n\
           status            show configured servers and resolved paths\n\
           check             validate config and test connectivity\n\
           sync              upload all non-excluded files\n\
           upload --file P   upload one file\n\
           watch [--interval N]  poll local files and upload changes\n\
           ls [--path P] [--json]  list a remote directory\n\n\
         config: .zed/deploy.json (local_root relative to project root)\n\
         protocols: local, sftp (ssh keys only, no passwords)"
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

fn select_server<'a>(config: &'a Config, requested: Option<String>) -> Result<&'a Server> {
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
            let command = "true";
            if dry_run {
                println!("ssh check: {command}");
            } else {
                run_ssh(server, command)?;
                println!("sftp connectivity ok");
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

    let mut child = Command::new("sftp")
        .args([
            "-o",
            "BatchMode=yes",
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
        .stderr(Stdio::piped())
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
            String::from_utf8_lossy(&output.stderr).trim()
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

    let output = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "-p",
            port.as_str(),
            destination.as_str(),
            remote_command,
        ])
        .output()
        .context("failed to run ssh")?;

    if !output.status.success() {
        bail!(
            "ssh failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
