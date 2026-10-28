use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Supported database drivers. Every driver is reached through an official
/// command-line client; no database driver crate is linked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Driver {
    Sqlite,
    Postgres,
    Mysql,
    MariaDb,
    SqlServer,
    Oracle,
    Mongodb,
    Redis,
}

impl Driver {
    pub const ALL: [Driver; 8] = [
        Driver::Sqlite,
        Driver::Postgres,
        Driver::Mysql,
        Driver::MariaDb,
        Driver::SqlServer,
        Driver::Oracle,
        Driver::Mongodb,
        Driver::Redis,
    ];

    pub fn parse(value: &str) -> Option<Driver> {
        Some(match value {
            "sqlite" => Driver::Sqlite,
            "postgres" => Driver::Postgres,
            "mysql" => Driver::Mysql,
            "mariadb" => Driver::MariaDb,
            "sqlserver" => Driver::SqlServer,
            "oracle" => Driver::Oracle,
            "mongodb" => Driver::Mongodb,
            "redis" => Driver::Redis,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Driver::Sqlite => "sqlite",
            Driver::Postgres => "postgres",
            Driver::Mysql => "mysql",
            Driver::MariaDb => "mariadb",
            Driver::SqlServer => "sqlserver",
            Driver::Oracle => "oracle",
            Driver::Mongodb => "mongodb",
            Driver::Redis => "redis",
        }
    }

    /// The official client binary used for this driver.
    pub fn client(self) -> &'static str {
        match self {
            Driver::Sqlite => "sqlite3",
            Driver::Postgres => "psql",
            Driver::Mysql | Driver::MariaDb => "mysql",
            Driver::SqlServer => "sqlcmd",
            Driver::Oracle => "sqlplus",
            Driver::Mongodb => "mongosh",
            Driver::Redis => "redis-cli",
        }
    }

    pub fn is_sql(self) -> bool {
        !matches!(self, Driver::Mongodb | Driver::Redis)
    }

    pub fn default_port(self) -> Option<u16> {
        match self {
            Driver::Sqlite => None,
            Driver::Postgres => Some(5432),
            Driver::Mysql | Driver::MariaDb => Some(3306),
            Driver::SqlServer => Some(1433),
            Driver::Oracle => Some(1521),
            Driver::Mongodb => Some(27017),
            Driver::Redis => Some(6379),
        }
    }
}

impl std::fmt::Display for Driver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub connections: Vec<Connection>,
}

/// SSH tunnel description. When `enabled` is true, every command opens a
/// temporary `ssh -N -L` process and reaches the database through
/// `127.0.0.1:<local_port>`; the process is terminated when the command ends.
/// Only key-based / agent authentication works (`BatchMode=yes`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshTunnel {
    #[serde(default)]
    pub enabled: bool,
    pub host: String,
    pub user: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    #[serde(default)]
    pub identity_file: Option<String>,
    #[serde(default)]
    pub local_port: Option<u16>,
}

fn default_ssh_port() -> u16 {
    22
}

impl SshTunnel {
    fn validate(&self, connection: &str) -> Result<()> {
        if self.host.trim().is_empty() {
            bail!("connection `{connection}` has an ssh_tunnel with an empty host");
        }
        if self.user.trim().is_empty() {
            bail!("connection `{connection}` has an ssh_tunnel with an empty user");
        }
        if self.port == 0 {
            bail!("connection `{connection}` has an ssh_tunnel with port 0");
        }
        if self.identity_file.as_deref() == Some("") {
            bail!("connection `{connection}` has an ssh_tunnel with an empty identity_file");
        }
        if self.local_port == Some(0) {
            bail!("connection `{connection}` has an ssh_tunnel with local_port 0");
        }
        Ok(())
    }
}

/// One configured connection. Passwords are never stored here: `password_env`
/// names an environment variable that is read at runtime.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub name: String,
    pub driver: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub database: Option<String>,
    #[serde(default)]
    pub password_env: Option<String>,
    #[serde(default)]
    pub socket: Option<String>,
    #[serde(default)]
    pub sslmode: Option<String>,
    #[serde(default)]
    pub ssl: Option<bool>,
    #[serde(default)]
    pub ssh_tunnel: Option<SshTunnel>,
    /// Refuse write statements on `query` and refuse `exec` entirely. SQLite
    /// databases are additionally opened with the `-readonly` flag.
    #[serde(default)]
    pub read_only: bool,
    /// Driver options merged into the client invocation (string → string).
    /// Unknown keys are kept but ignored; `--verbose-dsn` lists them.
    #[serde(default)]
    pub options: Option<BTreeMap<String, String>>,
    /// Password decoded from `url` by `normalize`. Internal: it survives the
    /// ssh-tunnel rewrite, which drops `url` in favour of host/port fields.
    #[serde(default)]
    pub resolved_password: Option<String>,
}

pub struct Store {
    pub connections: Vec<Connection>,
    pub path: PathBuf,
    pub base: PathBuf,
}

impl Store {
    pub fn load(explicit: Option<&Path>) -> Result<Store> {
        let path = match explicit {
            Some(path) => path.to_path_buf(),
            None => find_config()?,
        };
        let text = fs::read_to_string(&path)
            .with_context(|| format!("cannot read config {}", path.display()))?;
        let file: ConfigFile = serde_json::from_str(&text)
            .with_context(|| format!("invalid JSON in {}", path.display()))?;
        let mut connections = file.connections;
        if connections.is_empty() {
            bail!("config {} has no connections", path.display());
        }
        validate_all(&mut connections)?;
        Ok(Store {
            connections,
            base: base_dir(&path),
            path,
        })
    }

    pub fn get(&self, name: &str) -> Result<&Connection> {
        match self.connections.iter().find(|c| c.name == name) {
            Some(connection) => Ok(connection),
            None => {
                let names = self
                    .connections
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!(
                    "connection `{name}` not found in {} (available: {names})",
                    self.path.display()
                )
            }
        }
    }
}

fn validate_all(connections: &mut [Connection]) -> Result<()> {
    let mut seen: Vec<String> = Vec::with_capacity(connections.len());
    for connection in connections.iter_mut() {
        connection.normalize()?;
        connection.validate()?;
        if seen.contains(&connection.name) {
            bail!("duplicate connection name `{}`", connection.name);
        }
        seen.push(connection.name.clone());
    }
    Ok(())
}

impl Connection {
    pub fn driver(&self) -> Result<Driver> {
        Driver::parse(&self.driver).with_context(|| {
            format!(
                "connection `{}` has unsupported driver `{}` (expected {})",
                self.name,
                self.driver,
                driver_list()
            )
        })
    }

    /// Applies PhpStorm "url overrides settings above" semantics: when `url` is
    /// present and parseable, its host/port/user/password/database replace the
    /// individual fields. Scheme-less urls (Oracle's `user@host:port/service`)
    /// are passed through untouched.
    pub fn normalize(&mut self) -> Result<()> {
        let Some(url) = self
            .url
            .as_deref()
            .filter(|url| !url.is_empty() && url.contains("://"))
        else {
            return Ok(());
        };
        let Ok(driver) = self.driver() else {
            return Ok(());
        };
        let allowed: &[&str] = match driver {
            Driver::Postgres => &["postgres", "postgresql"],
            Driver::Mysql => &["mysql"],
            Driver::MariaDb => &["mysql", "mariadb"],
            Driver::SqlServer => &["sqlserver", "mssql"],
            Driver::Redis => &["redis", "rediss"],
            // MongoDB urls may list several hosts; they are consumed verbatim
            // by the mongosh invocation, so no field rewrite happens.
            Driver::Mongodb | Driver::Sqlite | Driver::Oracle => return Ok(()),
        };
        let parsed = url_to_conn(url)
            .with_context(|| format!("connection `{}` has an invalid url", self.name))?;
        if !allowed.contains(&parsed.scheme.as_str()) {
            bail!(
                "connection `{}`: url scheme `{}` does not match driver `{}` (expected {})",
                self.name,
                parsed.scheme,
                driver,
                allowed.join(" or ")
            );
        }
        self.host = Some(parsed.host);
        self.port = parsed.port;
        self.user = parsed.user;
        self.database = parsed.database;
        self.resolved_password = parsed.password;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            bail!("a connection has an empty `name`");
        }
        let driver = self.driver()?;
        if self.port == Some(0) {
            bail!("connection `{}` has port 0", self.name);
        }
        if self.password_env.as_deref() == Some("") {
            bail!("connection `{}` has an empty `password_env`", self.name);
        }

        match driver {
            Driver::Sqlite => {
                if self.path.as_deref().is_none_or(str::is_empty) {
                    bail!("sqlite connection `{}` needs a `path`", self.name);
                }
            }
            Driver::Postgres => {
                if self.url.as_deref().is_none_or(str::is_empty)
                    && self.host.as_deref().is_none_or(str::is_empty)
                {
                    bail!(
                        "postgres connection `{}` needs a `url` or `host`",
                        self.name
                    );
                }
            }
            Driver::Mysql | Driver::MariaDb => {
                if self.url.as_deref().is_none_or(str::is_empty)
                    && self.host.as_deref().is_none_or(str::is_empty)
                    && self.socket.as_deref().is_none_or(str::is_empty)
                {
                    bail!(
                        "{} connection `{}` needs a `url`, `host`, or `socket`",
                        driver,
                        self.name
                    );
                }
                if self.url.as_deref().is_none_or(str::is_empty)
                    && self.database.as_deref().is_none_or(str::is_empty)
                {
                    bail!("{} connection `{}` needs a `database`", driver, self.name);
                }
            }
            Driver::SqlServer => {
                if self.url.as_deref().is_none_or(str::is_empty)
                    && self.host.as_deref().is_none_or(str::is_empty)
                {
                    bail!(
                        "sqlserver connection `{}` needs a `url` or `host`",
                        self.name
                    );
                }
                if self.url.as_deref().is_none_or(str::is_empty)
                    && self.database.as_deref().is_none_or(str::is_empty)
                {
                    bail!("sqlserver connection `{}` needs a `database`", self.name);
                }
            }
            Driver::Oracle => {
                let has_url = self.url.as_deref().is_some_and(|url| !url.is_empty());
                let has_fields = self.host.as_deref().is_some_and(|host| !host.is_empty())
                    && self.user.as_deref().is_some_and(|user| !user.is_empty());
                if !has_url && !has_fields {
                    bail!(
                        "oracle connection `{}` needs a `url` or both `host` and `user`",
                        self.name
                    );
                }
            }
            Driver::Mongodb => {
                if self.url.as_deref().is_none_or(str::is_empty)
                    && self.host.as_deref().is_none_or(str::is_empty)
                {
                    bail!("mongodb connection `{}` needs a `url` or `host`", self.name);
                }
            }
            Driver::Redis => {
                if self.url.as_deref().is_none_or(str::is_empty)
                    && self.host.as_deref().is_none_or(str::is_empty)
                    && self.socket.as_deref().is_none_or(str::is_empty)
                {
                    bail!(
                        "redis connection `{}` needs a `url`, `host`, or `socket`",
                        self.name
                    );
                }
            }
        }

        if let Some(tunnel) = &self.ssh_tunnel {
            tunnel.validate(&self.name)?;
            if tunnel.enabled
                && !matches!(
                    driver,
                    Driver::Postgres | Driver::Mysql | Driver::MariaDb
                )
            {
                bail!(
                    "connection `{}`: ssh_tunnel.enabled is only supported for postgres, mysql, and mariadb drivers",
                    self.name
                );
            }
        }
        Ok(())
    }

    pub fn sqlite_path(&self, base: &Path) -> Result<PathBuf> {
        let raw = self
            .path
            .as_deref()
            .with_context(|| format!("sqlite connection `{}` has no path", self.name))?;
        let path = Path::new(raw);
        Ok(if path.is_absolute() {
            path.to_path_buf()
        } else {
            base.join(path)
        })
    }

    /// Reads the password from the environment variable named by `password_env`.
    /// Without `password_env`, the password decoded from `url` (or embedded in
    /// `url` when normalization was skipped) is used as a fallback.
    pub fn password(&self) -> Result<Option<String>> {
        match &self.password_env {
            None => Ok(self
                .resolved_password
                .clone()
                .or_else(|| self.url_password())),
            Some(var) => match env::var(var) {
                Ok(value) => Ok(Some(value)),
                Err(_) => bail!(
                    "environment variable `{var}` (password_env for `{}`) is not set",
                    self.name
                ),
            },
        }
    }

    /// Password embedded in a URL, if any. Used for warnings, redaction, and as
    /// a fallback when `password_env` is absent.
    pub fn url_password(&self) -> Option<String> {
        let url = self.url.as_deref()?;
        let authority = url.split_once("://")?.1.split(['/', '?', '#']).next()?;
        let userinfo = authority.rsplit_once('@')?.0;
        let (_, password) = userinfo.split_once(':')?;
        if password.is_empty() {
            None
        } else {
            Some(percent_decode(password))
        }
    }

    pub fn url_username(&self) -> Option<String> {
        let url = self.url.as_deref()?;
        let authority = url.split_once("://")?.1.split(['/', '?', '#']).next()?;
        let userinfo = authority.rsplit_once('@')?.0;
        let (user, _) = userinfo.split_once(':').unwrap_or((userinfo, ""));
        if user.is_empty() {
            None
        } else {
            Some(percent_decode(user))
        }
    }

    /// A redacted, human-readable target. Passwords never appear here.
    pub fn target(&self) -> String {
        let Ok(driver) = self.driver() else {
            return self.driver.clone();
        };
        match driver {
            Driver::Sqlite => self.path.clone().unwrap_or_default(),
            Driver::Postgres => self
                .url
                .clone()
                .map(|url| redact_url(&url))
                .unwrap_or_else(|| {
                    format!(
                        "{}@{}:{}/{}",
                        self.user.as_deref().unwrap_or(""),
                        self.host.as_deref().unwrap_or(""),
                        self.port.or(driver.default_port()).unwrap_or(5432),
                        self.database.as_deref().unwrap_or("")
                    )
                }),
            Driver::Mysql | Driver::MariaDb => {
                if let Some(url) = &self.url {
                    return redact_url(url);
                }
                let base = match (&self.host, &self.socket) {
                    (Some(host), Some(socket)) => format!(
                        "{}@{}:{}/{} (socket {})",
                        self.user.as_deref().unwrap_or(""),
                        host,
                        self.port.or(driver.default_port()).unwrap_or(3306),
                        self.database.as_deref().unwrap_or(""),
                        socket
                    ),
                    (None, Some(socket)) => format!(
                        "{}@socket:{}/{}",
                        self.user.as_deref().unwrap_or(""),
                        socket,
                        self.database.as_deref().unwrap_or("")
                    ),
                    _ => format!(
                        "{}@{}:{}/{}",
                        self.user.as_deref().unwrap_or(""),
                        self.host.as_deref().unwrap_or(""),
                        self.port.or(driver.default_port()).unwrap_or(3306),
                        self.database.as_deref().unwrap_or("")
                    ),
                };
                base
            }
            Driver::SqlServer => {
                self.url
                    .clone()
                    .map(|url| redact_url(&url))
                    .unwrap_or_else(|| {
                        format!(
                            "{}@{}:{}/{}",
                            self.user.as_deref().unwrap_or(""),
                            self.host.as_deref().unwrap_or(""),
                            self.port.or(driver.default_port()).unwrap_or(1433),
                            self.database.as_deref().unwrap_or("")
                        )
                    })
            }
            Driver::Oracle => self
                .url
                .clone()
                .map(|url| redact_url(&url))
                .unwrap_or_else(|| {
                    format!(
                        "{}@{}:{}/{}",
                        self.user.as_deref().unwrap_or(""),
                        self.host.as_deref().unwrap_or(""),
                        self.port.or(driver.default_port()).unwrap_or(1521),
                        self.database.as_deref().unwrap_or("")
                    )
                }),
            Driver::Mongodb => self
                .url
                .clone()
                .map(|url| redact_url(&url))
                .unwrap_or_else(|| {
                    format!(
                        "mongodb://{}:{}/{}",
                        self.host.as_deref().unwrap_or(""),
                        self.port.or(driver.default_port()).unwrap_or(27017),
                        self.database.as_deref().unwrap_or("")
                    )
                }),
            Driver::Redis => self
                .url
                .clone()
                .map(|url| redact_url(&url))
                .unwrap_or_else(|| match &self.socket {
                    Some(socket) => format!("redis+unix:{}", socket),
                    None => format!(
                        "redis://{}:{}",
                        self.host.as_deref().unwrap_or("127.0.0.1"),
                        self.port.or(driver.default_port()).unwrap_or(6379)
                    ),
                }),
        }
    }

    /// Non-fatal configuration warnings shown by `list`.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.url_password().is_some() {
            warnings.push("plaintext password in url; use password_env".to_string());
        }
        if let Some(tunnel) = &self.ssh_tunnel {
            if tunnel.enabled {
                warnings.push(format!(
                    "ssh_tunnel enabled: commands connect through {}@{}:{}",
                    tunnel.user, tunnel.host, tunnel.port
                ));
            } else {
                warnings.push("ssh_tunnel is configured but not active yet".to_string());
            }
        }
        warnings
    }
}

fn driver_list() -> String {
    Driver::ALL
        .iter()
        .map(|driver| driver.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn find_config() -> Result<PathBuf> {
    let cwd = env::current_dir().context("cannot determine the current directory")?;
    for dir in cwd.ancestors() {
        let candidate = dir.join(".zed").join("databases.json");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!(
        "no .zed/databases.json found in {} or any parent directory (use --config PATH)",
        cwd.display()
    );
}

/// Relative SQLite paths are resolved against the project root (the parent of
/// `.zed`), not against `.zed` itself.
fn base_dir(config_path: &Path) -> PathBuf {
    let parent = config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if parent.file_name().map(|n| n == ".zed").unwrap_or(false) {
        parent
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    } else {
        parent.to_path_buf()
    }
}

/// Replaces the password portion of a URL with `***` for display.
pub fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let Some((userinfo, host)) = authority.rsplit_once('@') else {
        return url.to_string();
    };
    match userinfo.split_once(':') {
        Some((user, password)) if !password.is_empty() => {
            format!("{scheme}://{user}:***@{host}{tail}")
        }
        _ => url.to_string(),
    }
}

/// Removes the password from a URL, returning the cleaned URL and the decoded
/// password. The username is preserved.
pub fn strip_url_password(url: &str) -> (String, Option<String>) {
    let Some((scheme, rest)) = url.split_once("://") else {
        return (url.to_string(), None);
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let Some((userinfo, host)) = authority.rsplit_once('@') else {
        return (url.to_string(), None);
    };
    match userinfo.split_once(':') {
        Some((user, password)) if !password.is_empty() => {
            let prefix = if user.is_empty() {
                String::new()
            } else {
                format!("{user}@")
            };
            (
                format!("{scheme}://{prefix}{host}{tail}"),
                Some(percent_decode(password)),
            )
        }
        _ => (url.to_string(), None),
    }
}

/// Removes the whole userinfo section from a URL.
pub fn strip_url_credentials(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let Some((_, host)) = authority.rsplit_once('@') else {
        return url.to_string();
    };
    format!("{scheme}://{host}{tail}")
}

/// Appends `key=value` to a URL query string unless the key is already present.
pub fn add_query_param(url: &str, key: &str, value: &str) -> String {
    if url.split_once('?').is_some_and(|(_, query)| {
        query
            .split('&')
            .any(|pair| pair.starts_with(&format!("{key}=")))
    }) {
        return url.to_string();
    }
    let (base, fragment) = match url.split_once('#') {
        Some((base, fragment)) => (base, Some(fragment)),
        None => (url, None),
    };
    let separator = if base.contains('?') { '&' } else { '?' };
    let mut result = format!("{base}{separator}{key}={value}");
    if let Some(fragment) = fragment {
        result.push('#');
        result.push_str(fragment);
    }
    result
}

/// One URL parsed into connection-shaped fields. Pure: no I/O, no driver
/// lookup — callers decide which schemes are acceptable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedConn {
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub database: Option<String>,
    pub params: Vec<(String, String)>,
}

/// Parses `scheme://[user[:password]@]host[:port][/database][?params][#frag]`
/// into `ParsedConn`. IPv6 hosts in brackets, percent-escaped userinfo, and
/// query parameters are handled. An empty user (`redis://:pw@host`) becomes
/// `user: None` so password-only urls keep working.
pub fn url_to_conn(url: &str) -> Result<ParsedConn> {
    let (scheme, rest) = url
        .split_once("://")
        .with_context(|| format!("url must include a scheme: {url}"))?;
    if scheme.is_empty() {
        bail!("url has an empty scheme: {url}");
    }
    let (authority, tail) = match rest.split_once('/') {
        Some((authority, tail)) => (authority, Some(tail)),
        None => (rest, None),
    };
    let database = tail
        .and_then(|tail| tail.split(['?', '#']).next())
        .filter(|database| !database.is_empty())
        .map(str::to_string);
    let query = tail
        .and_then(|tail| tail.split_once('?').map(|(_, query)| query))
        .map(|query| query.split('#').next().unwrap_or(query))
        .unwrap_or("");
    let mut params = Vec::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        params.push((percent_decode(key), percent_decode(value)));
    }

    let (userinfo, host_port) = match authority.rsplit_once('@') {
        Some((userinfo, host_port)) => (Some(userinfo), host_port),
        None => (None, authority),
    };
    let (user, password) = match userinfo {
        Some(userinfo) => match userinfo.split_once(':') {
            Some((user, password)) => (Some(percent_decode(user)), Some(percent_decode(password))),
            None => (Some(percent_decode(userinfo)), None),
        },
        None => (None, None),
    };
    let user = user.filter(|user| !user.is_empty());
    let (host, port) = split_authority(host_port)?;
    if host.is_empty() {
        bail!("url is missing a host");
    }
    Ok(ParsedConn {
        scheme: scheme.to_string(),
        host,
        port,
        user,
        password,
        database,
        params,
    })
}

fn split_authority(value: &str) -> Result<(String, Option<u16>)> {
    if let Some(rest) = value.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .context("url has an unterminated IPv6 host")?;
        let port = match tail.strip_prefix(':') {
            Some(port) => Some(port.parse().context("url has an invalid port")?),
            None => None,
        };
        return Ok((host.to_string(), port));
    }
    match value.rsplit_once(':') {
        Some((host, port)) => Ok((
            host.to_string(),
            Some(port.parse().context("url has an invalid port")?),
        )),
        None => Ok((value.to_string(), None)),
    }
}

pub fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                out.push(high * 16 + low);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<Vec<Connection>> {
        let mut file: ConfigFile = serde_json::from_str(json)?;
        validate_all(&mut file.connections)?;
        Ok(file.connections)
    }

    #[test]
    fn parses_every_driver_and_defaults() {
        let connections = parse(include_str!(
            "../../../fixtures/db-drivers/.zed/databases.json"
        ))
        .unwrap();
        assert_eq!(connections.len(), 8);
        let drivers = connections
            .iter()
            .map(|connection| connection.driver().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(drivers, Driver::ALL.to_vec());
        let pg = &connections[1];
        assert_eq!(pg.sslmode.as_deref(), Some("require"));
        let tunnel = pg.ssh_tunnel.as_ref().unwrap();
        assert_eq!(tunnel.port, 22);
        let my = &connections[2];
        assert_eq!(my.ssl, Some(true));
        assert_eq!(my.socket.as_deref(), Some("/tmp/mysql.sock"));
        let cache = &connections[7];
        assert_eq!(cache.port, Some(6379));
        assert_eq!(Driver::Mysql.default_port(), Some(3306));
        assert_eq!(Driver::Oracle.default_port(), Some(1521));
    }

    #[test]
    fn backwards_compatible_shape_still_parses() {
        let connections =
            parse(r#"{"connections":[{"name":"app","driver":"sqlite","path":"app.sqlite"}]}"#)
                .unwrap();
        assert_eq!(connections[0].path.as_deref(), Some("app.sqlite"));
    }

    #[test]
    fn rejects_unknown_driver() {
        let error = parse(r#"{"connections":[{"name":"x","driver":"db2","host":"h"}]}"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unsupported driver `db2`"));
        assert!(error.contains("postgres"));
    }

    #[test]
    fn rejects_plaintext_password_field() {
        let error = parse(
            r#"{"connections":[{"name":"app","driver":"sqlite","path":"app.sqlite","password":"nope"}]}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unknown field"));
    }

    #[test]
    fn rejects_unknown_ssh_tunnel_field() {
        let error = parse(
            r#"{"connections":[{"name":"pg","driver":"postgres","host":"h",
                "ssh_tunnel":{"host":"b","user":"u","password":"nope"}}]}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unknown field"));
    }

    #[test]
    fn requires_driver_specific_fields() {
        assert!(parse(r#"{"connections":[{"name":"x","driver":"sqlite"}]}"#)
            .unwrap_err()
            .to_string()
            .contains("needs a `path`"));
        assert!(
            parse(r#"{"connections":[{"name":"x","driver":"postgres"}]}"#)
                .unwrap_err()
                .to_string()
                .contains("needs a `url` or `host`")
        );
        assert!(
            parse(r#"{"connections":[{"name":"x","driver":"mysql","host":"h"}]}"#)
                .unwrap_err()
                .to_string()
                .contains("needs a `database`")
        );
        assert!(parse(
            r#"{"connections":[{"name":"x","driver":"redis","socket":"/tmp/redis.sock"}]}"#
        )
        .is_ok());
    }

    #[test]
    fn rejects_duplicate_names_and_empty_names() {
        let error = parse(
            r#"{"connections":[
                {"name":"a","driver":"sqlite","path":"a.sqlite"},
                {"name":"a","driver":"sqlite","path":"b.sqlite"}
            ]}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("duplicate connection name"));
        let error = parse(r#"{"connections":[{"name":" ","driver":"sqlite","path":"a"}]}"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("empty `name`"));
    }

    #[test]
    fn redacts_url_passwords() {
        assert_eq!(
            redact_url("postgres://app:s3cret@db:5432/app"),
            "postgres://app:***@db:5432/app"
        );
        assert_eq!(
            redact_url("mysql://app@localhost:3306/app"),
            "mysql://app@localhost:3306/app"
        );
        assert_eq!(redact_url("sqlite-file"), "sqlite-file");
    }

    #[test]
    fn targets_never_show_passwords() {
        let connections = parse(include_str!(
            "../../../fixtures/db-drivers/.zed/databases.json"
        ))
        .unwrap();
        for connection in &connections {
            let target = connection.target();
            assert!(!target.contains("s3cret"), "{target}");
            assert!(!target.contains("password"), "{target}");
        }
        let connection = parse(
            r#"{"connections":[{"name":"pg","driver":"postgres",
                "url":"postgres://app:s3cret@db:5432/app"}]}"#,
        )
        .unwrap()
        .remove(0);
        assert_eq!(connection.target(), "postgres://app:***@db:5432/app");
        assert_eq!(connection.url_password().as_deref(), Some("s3cret"));
    }

    #[test]
    fn strips_url_credentials_for_client_arguments() {
        let (url, password) = strip_url_password("mysql://app:p%40ss@db:3306/app");
        assert_eq!(url, "mysql://app@db:3306/app");
        assert_eq!(password.as_deref(), Some("p@ss"));
        assert_eq!(
            strip_url_credentials("mongodb://app:secret@localhost:27017/db"),
            "mongodb://localhost:27017/db"
        );
        let (url, password) = strip_url_password("redis://:secret@localhost:6379");
        assert_eq!(url, "redis://localhost:6379");
        assert_eq!(password.as_deref(), Some("secret"));
    }

    #[test]
    fn adds_query_parameters_once() {
        assert_eq!(
            add_query_param("postgres://h/db", "sslmode", "require"),
            "postgres://h/db?sslmode=require"
        );
        assert_eq!(
            add_query_param("postgres://h/db?x=1", "sslmode", "require"),
            "postgres://h/db?x=1&sslmode=require"
        );
        assert_eq!(
            add_query_param("postgres://h/db?sslmode=disable", "sslmode", "require"),
            "postgres://h/db?sslmode=disable"
        );
    }

    #[test]
    fn password_env_is_validated_on_use() {
        let connection = Connection {
            name: "pg".to_string(),
            driver: "postgres".to_string(),
            path: None,
            url: Some("postgres://app@localhost/app".to_string()),
            host: None,
            port: None,
            user: None,
            database: None,
            password_env: Some("PHP_DB_TEST_UNSET_VAR".to_string()),
            socket: None,
            sslmode: None,
            ssl: None,
            ssh_tunnel: None,
            read_only: false,
            options: None,
            resolved_password: None,
        };
        let error = connection.password().unwrap_err().to_string();
        assert!(error.contains("PHP_DB_TEST_UNSET_VAR"));
    }

    #[test]
    fn resolves_relative_paths_against_project_root() {
        let base = base_dir(Path::new("/project/.zed/databases.json"));
        assert_eq!(base, Path::new("/project"));
        assert_eq!(
            base_dir(Path::new("/tmp/databases.json")),
            Path::new("/tmp")
        );
    }

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(percent_decode("p%40ss%20word"), "p@ss word");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn url_to_conn_parses_mysql_url() {
        let parsed = url_to_conn("mysql://news:pw@localhost:3306/newsystem").unwrap();
        assert_eq!(parsed.scheme, "mysql");
        assert_eq!(parsed.host, "localhost");
        assert_eq!(parsed.port, Some(3306));
        assert_eq!(parsed.user.as_deref(), Some("news"));
        assert_eq!(parsed.password.as_deref(), Some("pw"));
        assert_eq!(parsed.database.as_deref(), Some("newsystem"));
        assert!(parsed.params.is_empty());
    }

    #[test]
    fn url_to_conn_parses_postgres_url_with_params() {
        let parsed = url_to_conn("postgres://u@h:5432/db?sslmode=require").unwrap();
        assert_eq!(parsed.scheme, "postgres");
        assert_eq!(parsed.host, "h");
        assert_eq!(parsed.port, Some(5432));
        assert_eq!(parsed.user.as_deref(), Some("u"));
        assert_eq!(parsed.password, None);
        assert_eq!(parsed.database.as_deref(), Some("db"));
        assert_eq!(
            parsed.params,
            vec![("sslmode".to_string(), "require".to_string())]
        );
    }

    #[test]
    fn url_to_conn_rejects_invalid_urls() {
        assert!(url_to_conn("host:5432/db").is_err());
        assert!(url_to_conn("mysql://h:notaport/db").is_err());
        assert!(url_to_conn("mysql:///db").is_err());
        let parsed = url_to_conn("mysql://h/db").unwrap();
        assert_eq!(parsed.port, None);
    }

    #[test]
    fn url_to_conn_handles_ipv6_and_empty_user() {
        let parsed = url_to_conn("mysql://u:p@[::1]:3306/db").unwrap();
        assert_eq!(parsed.host, "::1");
        assert_eq!(parsed.port, Some(3306));
        let parsed = url_to_conn("redis://:secret@localhost:6379").unwrap();
        assert_eq!(parsed.user, None);
        assert_eq!(parsed.password.as_deref(), Some("secret"));
    }

    #[test]
    fn url_overrides_individual_fields_at_load() {
        let connections = parse(
            r#"{"connections":[{"name":"my","driver":"mysql",
                "host":"wrong","port":1,"user":"wrong","database":"wrong",
                "url":"mysql://news:pw@localhost:3306/newsystem"}]}"#,
        )
        .unwrap();
        let my = &connections[0];
        assert_eq!(my.host.as_deref(), Some("localhost"));
        assert_eq!(my.port, Some(3306));
        assert_eq!(my.user.as_deref(), Some("news"));
        assert_eq!(my.database.as_deref(), Some("newsystem"));
        assert_eq!(my.url_password().as_deref(), Some("pw"));
    }

    #[test]
    fn new_fields_parse_with_defaults() {
        let error = parse(
            r#"{"connections":[{"name":"ro","driver":"sqlite","path":"a.sqlite",
                "ssh_tunnel":{"enabled":true,"host":"b","user":"u","local_port":15432}}]}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("only supported for postgres, mysql, and mariadb"));

        let connections = parse(
            r#"{"connections":[{"name":"ro","driver":"sqlite","path":"a.sqlite",
                "read_only":true,
                "options":{"charset":"utf8mb4"},
                "ssh_tunnel":{"enabled":false,"host":"b","user":"u","local_port":15432}}]}"#,
        )
        .unwrap();
        let ro = &connections[0];
        assert!(ro.read_only);
        assert_eq!(
            ro.options.as_ref().unwrap().get("charset"),
            Some(&"utf8mb4".to_string())
        );
        let tunnel = ro.ssh_tunnel.as_ref().unwrap();
        assert!(!tunnel.enabled);
        assert_eq!(tunnel.local_port, Some(15432));
        let plain = parse(r#"{"connections":[{"name":"x","driver":"sqlite","path":"p"}]}"#)
            .unwrap()
            .remove(0);
        assert!(!plain.read_only);
        assert!(plain.options.is_none());
    }

    #[test]
    fn enabled_tunnel_requires_tcp_driver() {
        let error = parse(
            r#"{"connections":[{"name":"ms","driver":"sqlserver","host":"h","database":"d",
                "ssh_tunnel":{"enabled":true,"host":"b","user":"u"}}]}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("only supported for postgres, mysql, and mariadb"));
        // Disabled tunnels stay inert on any driver (metadata only).
        assert!(parse(
            r#"{"connections":[{"name":"ms","driver":"sqlserver","host":"h","database":"d",
                "ssh_tunnel":{"enabled":false,"host":"b","user":"u"}}]}"#
        )
        .is_ok());
    }

    #[test]
    fn local_port_zero_is_rejected() {
        let error = parse(
            r#"{"connections":[{"name":"pg","driver":"postgres","host":"h",
                "ssh_tunnel":{"enabled":true,"host":"b","user":"u","local_port":0}}]}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("local_port 0"));
    }

    #[test]
    fn db_test_fixture_with_new_fields_parses() {
        let connections = parse(include_str!(
            "../../../fixtures/db-test/.zed/databases.json"
        ))
        .unwrap();
        let ro = connections
            .iter()
            .find(|c| c.name == "app-ro")
            .expect("app-ro connection");
        assert!(ro.read_only);
        assert!(ro.options.is_some());
        assert!(ro.ssh_tunnel.is_some());
    }

    #[test]
    fn scheme_mismatch_between_url_and_driver_is_rejected() {
        let error = parse(
            r#"{"connections":[{"name":"my","driver":"mariadb","url":"postgres://u@h/db"}]}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("does not match driver"));
    }
}
