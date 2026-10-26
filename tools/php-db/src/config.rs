use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub connections: Vec<Connection>,
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
    pub password_env: Option<String>,
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
        for connection in &file.connections {
            connection.validate()?;
        }
        if file.connections.is_empty() {
            bail!("config {} has no connections", path.display());
        }
        Ok(Store {
            connections: file.connections,
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

impl Connection {
    fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            bail!("a connection has an empty `name`");
        }
        match self.driver.as_str() {
            "sqlite" => {
                if self.path.as_deref().is_none_or(str::is_empty) {
                    bail!("sqlite connection `{}` needs a `path`", self.name);
                }
            }
            "postgres" | "mysql" => {
                if self.url.as_deref().is_none_or(str::is_empty) {
                    bail!("{} connection `{}` needs a `url`", self.driver, self.name);
                }
            }
            other => bail!(
                "connection `{}` has unsupported driver `{other}` (expected sqlite, postgres, mysql)",
                self.name
            ),
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
    pub fn password(&self) -> Result<Option<String>> {
        match &self.password_env {
            None => Ok(None),
            Some(var) => match env::var(var) {
                Ok(value) => Ok(Some(value)),
                Err(_) => bail!(
                    "environment variable `{var}` (password_env for `{}`) is not set",
                    self.name
                ),
            },
        }
    }

    /// Password embedded in a URL, if any. Used only for warnings and as a
    /// fallback; `password_env` is preferred.
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
    )
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
    fn resolves_relative_paths_against_project_root() {
        let base = base_dir(Path::new("/project/.zed/databases.json"));
        assert_eq!(base, Path::new("/project"));
        assert_eq!(
            base_dir(Path::new("/tmp/databases.json")),
            Path::new("/tmp")
        );
    }

    #[test]
    fn config_rejects_plaintext_password_field() {
        let json = r#"{"connections":[
            {"name":"app","driver":"sqlite","path":"app.sqlite","password":"nope"}
        ]}"#;
        let err = serde_json::from_str::<ConfigFile>(json).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn password_env_is_validated_on_use() {
        let connection: Connection = serde_json::from_str(
            r#"{"name":"pg","driver":"postgres","url":"postgres://app@localhost/app","password_env":"PHP_DB_TEST_UNSET_VAR"}"#,
        )
        .unwrap();
        let err = connection.password().unwrap_err();
        assert!(err.to_string().contains("PHP_DB_TEST_UNSET_VAR"));
    }

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(percent_decode("p%40ss%20word"), "p@ss word");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("100%"), "100%");
    }
}
