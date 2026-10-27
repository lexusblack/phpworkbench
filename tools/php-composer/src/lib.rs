use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const USAGE: &str = "\
php-composer - Composer bridge for the IDE

Usage:
  php-composer [--dir PATH] <command> [options]

Commands:
  list [--json]                 Direct required packages
  outdated [--json]             Direct outdated packages
  validate [--json]             Validate composer.json
  show <package> [--json]       Show package details
  scripts [--json]              List composer scripts
  run <script> [--json]         Run a composer script
  require <pkg>[:constraint] [--dev] [--dry-run] [--allow-write] [--json]
  remove <pkg> [--dry-run] [--allow-write] [--json]

Options:
  --dir PATH       Project directory (default: .). Walks up for composer.json.
  --json           Emit normalized JSON.
  --dev            Add the requirement to require-dev (require only).
  --dry-run        Preview require/remove without writing.
  --allow-write    Permit require/remove without --dry-run.
  -h, --help       Show this help.

Environment:
  PHP_COMPOSER_BIN  Composer executable (default: composer on PATH).

Exit codes:
  0  success
  1  operation failed (invalid composer.json, failed require/remove)
  2  usage, tool, or configuration error (including composer missing)
  N  run propagates the Composer script's exit code
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub dir: PathBuf,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandInvocation {
    pub options: Options,
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    List,
    Outdated,
    Validate,
    Show {
        package: String,
    },
    Scripts,
    Run {
        script: String,
    },
    Require {
        spec: String,
        dev: bool,
        dry_run: bool,
        allow_write: bool,
    },
    Remove {
        package: String,
        dry_run: bool,
        allow_write: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    Help,
    Command(CommandInvocation),
}

pub fn parse_args<I>(args: I) -> Result<Invocation>
where
    I: IntoIterator<Item = String>,
{
    let args: Vec<String> = args.into_iter().collect();
    let mut dir: Option<PathBuf> = None;
    let mut json = false;
    let mut dev = false;
    let mut dry_run = false;
    let mut allow_write = false;
    let mut after_double_dash = false;
    let mut command: Option<String> = None;
    let mut operands: Vec<String> = Vec::new();

    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if !after_double_dash {
            match arg.as_str() {
                "-h" | "--help" => return Ok(Invocation::Help),
                "--" => {
                    after_double_dash = true;
                    index += 1;
                    continue;
                }
                "--json" => {
                    json = true;
                    index += 1;
                    continue;
                }
                "--dev" => {
                    dev = true;
                    index += 1;
                    continue;
                }
                "--dry-run" => {
                    dry_run = true;
                    index += 1;
                    continue;
                }
                "--allow-write" => {
                    allow_write = true;
                    index += 1;
                    continue;
                }
                "--dir" => {
                    let value = args
                        .get(index + 1)
                        .filter(|value| !value.is_empty())
                        .context("--dir requires a path")?;
                    dir = Some(PathBuf::from(value));
                    index += 2;
                    continue;
                }
                _ => {}
            }
            if let Some(value) = arg.strip_prefix("--dir=") {
                if value.is_empty() {
                    bail!("--dir requires a path");
                }
                dir = Some(PathBuf::from(value));
                index += 1;
                continue;
            }
            if arg.starts_with('-') && arg != "-" {
                bail!("unknown option: {arg}");
            }
        }

        if command.is_none() {
            command = Some(arg.clone());
        } else {
            operands.push(arg.clone());
        }
        index += 1;
    }

    let command = command.context("missing command (try --help)")?;
    let options = Options {
        dir: dir.unwrap_or_else(|| PathBuf::from(".")),
        json,
    };

    let command = match command.as_str() {
        "list" => {
            reject_modification_flags(dev, dry_run, allow_write, "list")?;
            expect_operands(&operands, 0, "list")?;
            Command::List
        }
        "outdated" => {
            reject_modification_flags(dev, dry_run, allow_write, "outdated")?;
            expect_operands(&operands, 0, "outdated")?;
            Command::Outdated
        }
        "validate" => {
            reject_modification_flags(dev, dry_run, allow_write, "validate")?;
            expect_operands(&operands, 0, "validate")?;
            Command::Validate
        }
        "show" => {
            reject_modification_flags(dev, dry_run, allow_write, "show")?;
            expect_operands(&operands, 1, "show <package>")?;
            validate_package_name(&operands[0])?;
            Command::Show {
                package: operands[0].clone(),
            }
        }
        "scripts" => {
            reject_modification_flags(dev, dry_run, allow_write, "scripts")?;
            expect_operands(&operands, 0, "scripts")?;
            Command::Scripts
        }
        "run" => {
            reject_modification_flags(dev, dry_run, allow_write, "run")?;
            expect_operands(&operands, 1, "run <script>")?;
            validate_script_name(&operands[0])?;
            Command::Run {
                script: operands[0].clone(),
            }
        }
        "require" => {
            expect_operands(&operands, 1, "require <pkg>[:constraint]")?;
            validate_require_spec(&operands[0])?;
            Command::Require {
                spec: operands[0].clone(),
                dev,
                dry_run,
                allow_write,
            }
        }
        "remove" => {
            if dev {
                bail!("--dev is only valid with require");
            }
            expect_operands(&operands, 1, "remove <package>")?;
            validate_package_name(&operands[0])?;
            Command::Remove {
                package: operands[0].clone(),
                dry_run,
                allow_write,
            }
        }
        other => bail!("unknown command: {other} (try --help)"),
    };

    Ok(Invocation::Command(CommandInvocation { options, command }))
}

fn reject_modification_flags(
    dev: bool,
    dry_run: bool,
    allow_write: bool,
    command: &str,
) -> Result<()> {
    if dev {
        bail!("--dev is only valid with require");
    }
    if dry_run {
        bail!("--dry-run is only valid with require/remove (got {command})");
    }
    if allow_write {
        bail!("--allow-write is only valid with require/remove (got {command})");
    }
    Ok(())
}

fn expect_operands(operands: &[String], expected: usize, usage: &str) -> Result<()> {
    if operands.len() != expected {
        if expected == 0 {
            bail!("unexpected argument for {usage}: {}", operands[0]);
        }
        bail!("usage: php-composer {usage}");
    }
    Ok(())
}

pub fn validate_package_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("package name must not be empty");
    }
    if name.starts_with('-') {
        bail!("invalid package name (must not start with '-'): {name}");
    }
    if !name.contains('/') {
        bail!("invalid package name (expected vendor/name): {name}");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '/'))
    {
        bail!("invalid package name: {name}");
    }
    Ok(())
}

pub fn validate_require_spec(spec: &str) -> Result<()> {
    if spec.starts_with('-') {
        bail!("invalid package spec (must not start with '-'): {spec}");
    }
    let (name, constraint) = match spec.split_once(':') {
        Some((name, constraint)) => (name, Some(constraint)),
        None => (spec, None),
    };
    validate_package_name(name)?;
    if let Some(constraint) = constraint {
        if constraint.trim().is_empty() {
            bail!("invalid package spec (empty constraint): {spec}");
        }
    }
    Ok(())
}

pub fn validate_script_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("script name must not be empty");
    }
    if name.starts_with('-') {
        bail!("invalid script name (must not start with '-'): {name}");
    }
    if name.chars().any(char::is_whitespace) {
        bail!("invalid script name (must not contain whitespace): {name}");
    }
    Ok(())
}

pub fn find_project(dir: &Path) -> Result<PathBuf> {
    let start = fs::canonicalize(dir)
        .with_context(|| format!("directory does not exist: {}", dir.display()))?;
    if !start.is_dir() {
        bail!("not a directory: {}", start.display());
    }

    let mut current = start.as_path();
    loop {
        if current.join("composer.json").is_file() {
            return Ok(current.to_path_buf());
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    bail!(
        "no composer.json found in {} or any parent directory",
        start.display()
    )
}

#[derive(Debug, Deserialize, Default)]
struct ManifestFile {
    #[serde(default)]
    require: BTreeMap<String, Value>,
    #[serde(default, rename = "require-dev")]
    require_dev: BTreeMap<String, Value>,
    #[serde(default)]
    scripts: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default)]
pub struct Manifest {
    require: BTreeMap<String, Value>,
    require_dev: BTreeMap<String, Value>,
    scripts: BTreeMap<String, Value>,
}

impl Manifest {
    pub fn constraint_for(&self, name: &str) -> Option<String> {
        self.require
            .get(name)
            .or_else(|| self.require_dev.get(name))
            .map(constraint_string)
    }
}

pub fn parse_manifest(text: &str) -> Result<Manifest> {
    let file: ManifestFile = serde_json::from_str(text).context("invalid composer.json")?;
    Ok(Manifest {
        require: file.require,
        require_dev: file.require_dev,
        scripts: file.scripts,
    })
}

pub fn load_manifest(root: &Path) -> Result<Manifest> {
    let path = root.join("composer.json");
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    parse_manifest(&text).with_context(|| format!("invalid {}", path.display()))
}

fn constraint_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub version: Option<String>,
    pub constraint: String,
    pub dev: bool,
}

#[derive(Debug, Clone, Default)]
pub struct LockFile {
    versions: BTreeMap<String, String>,
}

impl LockFile {
    fn version(&self, name: &str) -> Option<String> {
        self.versions.get(name).cloned()
    }
}

#[derive(Debug, Deserialize, Default)]
struct LockFileRaw {
    #[serde(default)]
    packages: Vec<LockPackage>,
    #[serde(default, rename = "packages-dev")]
    packages_dev: Vec<LockPackage>,
}

#[derive(Debug, Deserialize)]
struct LockPackage {
    name: String,
    #[serde(default)]
    version: Option<String>,
}

pub fn parse_lock(text: &str) -> Result<LockFile> {
    let raw: LockFileRaw = serde_json::from_str(text).context("invalid composer.lock")?;
    let mut versions = BTreeMap::new();
    for package in raw.packages.into_iter().chain(raw.packages_dev) {
        if let Some(version) = package.version {
            versions.insert(package.name, version);
        }
    }
    Ok(LockFile { versions })
}

pub fn load_lock(root: &Path) -> Result<LockFile> {
    let path = root.join("composer.lock");
    if !path.is_file() {
        return Ok(LockFile::default());
    }
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    parse_lock(&text).with_context(|| format!("invalid {}", path.display()))
}

pub fn direct_requirements(manifest: &Manifest, lock: &LockFile) -> Vec<Package> {
    let mut packages: BTreeMap<String, Package> = BTreeMap::new();
    for (requirements, dev) in [(&manifest.require, false), (&manifest.require_dev, true)] {
        for (name, value) in requirements {
            if !name.contains('/') {
                continue;
            }
            let package = Package {
                name: name.clone(),
                version: lock.version(name),
                constraint: constraint_string(value),
                dev,
            };
            packages.entry(name.clone()).or_insert(package);
        }
    }
    packages.into_values().collect()
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Script {
    pub name: String,
    pub commands: Vec<String>,
}

pub fn script_list(manifest: &Manifest) -> Vec<Script> {
    manifest
        .scripts
        .iter()
        .map(|(name, value)| Script {
            name: name.clone(),
            commands: script_commands(value),
        })
        .collect()
}

fn script_commands(value: &Value) -> Vec<String> {
    match value {
        Value::String(command) => vec![command.clone()],
        Value::Array(items) => items.iter().flat_map(script_commands).collect(),
        Value::Null => Vec::new(),
        other => vec![other.to_string()],
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OutdatedPackage {
    pub name: String,
    pub installed: String,
    pub latest: Option<String>,
    pub constraint: Option<String>,
}

pub fn normalize_outdated(value: &Value, manifest: &Manifest) -> Result<Vec<OutdatedPackage>> {
    let items = if let Some(items) = value.as_array() {
        items
    } else if let Some(items) = value.get("installed").and_then(Value::as_array) {
        items
    } else {
        bail!("unexpected composer outdated output (expected a JSON array)");
    };

    let mut packages = Vec::with_capacity(items.len());
    for item in items {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .context("composer outdated entry is missing name")?;
        let installed = item
            .get("version")
            .and_then(Value::as_str)
            .or_else(|| item.get("installed").and_then(Value::as_str))
            .unwrap_or("");
        let latest = item
            .get("latest")
            .and_then(Value::as_str)
            .map(str::to_string);
        packages.push(OutdatedPackage {
            name: name.to_string(),
            installed: installed.to_string(),
            latest,
            constraint: manifest.constraint_for(name),
        });
    }
    packages.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(packages)
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ValidationReport {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Errors,
    Warnings,
}

pub fn parse_validate(stdout: &str, stderr: &str, success: bool) -> ValidationReport {
    let combined = format!("{stdout}\n{stderr}");
    let clean = strip_ansi(&combined);
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut panel = Vec::new();
    let mut section = None;

    for line in clean.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            let heading = line.to_lowercase();
            section = if heading.contains("warning") {
                Some(Section::Warnings)
            } else if heading.contains("error") {
                Some(Section::Errors)
            } else {
                None
            };
            continue;
        }
        if let Some(message) = line.strip_prefix("- ") {
            let message = message.trim().to_string();
            match section {
                Some(Section::Warnings) => warnings.push(message),
                Some(Section::Errors) => errors.push(message),
                None => panel.push(message),
            }
            continue;
        }
        if line.starts_with("In ") && line.ends_with(':') {
            continue;
        }
        if line.starts_with("validate [") {
            break;
        }
        if line.starts_with("Composer could not detect the root package") {
            continue;
        }
        if line.starts_with("Run the \"diagnose\"") {
            continue;
        }
        if line.starts_with("./composer.json is valid") {
            continue;
        }
        panel.push(line.to_string());
    }

    if !success && errors.is_empty() {
        let details = panel.join(" ");
        if details.is_empty() {
            errors.push("composer validate failed".to_string());
        } else {
            errors.push(details);
        }
    }

    let valid = success && errors.is_empty();
    ValidationReport {
        valid,
        errors,
        warnings,
    }
}

fn strip_ansi(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            while let Some(&next) = chars.peek() {
                chars.next();
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }
        output.push(character);
    }
    output
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PackageDetails {
    pub name: String,
    pub versions: Vec<String>,
    pub description: Option<String>,
    pub requires: BTreeMap<String, String>,
}

pub fn normalize_show(value: &Value) -> Result<PackageDetails> {
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .context("composer show output is missing name")?
        .to_string();

    let versions = if let Some(items) = value.get("versions").and_then(Value::as_array) {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    } else if let Some(version) = value.get("version").and_then(Value::as_str) {
        vec![version.to_string()]
    } else {
        Vec::new()
    };

    let description = value
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string);

    let requires = value
        .get("requires")
        .and_then(Value::as_object)
        .map(|requires| {
            requires
                .iter()
                .map(|(name, constraint)| (name.clone(), constraint_string(constraint)))
                .collect()
        })
        .unwrap_or_default();

    Ok(PackageDetails {
        name,
        versions,
        description,
        requires,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(text: &str) -> Manifest {
        parse_manifest(text).unwrap()
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn temp_dir(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "php-composer-lib-{name}-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn parses_global_and_command_local_dir() {
        let invocation = parse_args(strings(&["--dir", "/tmp/a", "list", "--json"])).unwrap();
        let Invocation::Command(invocation) = invocation else {
            panic!("expected command");
        };
        assert_eq!(invocation.options.dir, PathBuf::from("/tmp/a"));
        assert!(invocation.options.json);
        assert_eq!(invocation.command, Command::List);

        let invocation = parse_args(strings(&["list", "--dir=/tmp/b"])).unwrap();
        let Invocation::Command(invocation) = invocation else {
            panic!("expected command");
        };
        assert_eq!(invocation.options.dir, PathBuf::from("/tmp/b"));
        assert!(!invocation.options.json);
    }

    #[test]
    fn parses_help() {
        assert_eq!(parse_args(strings(&["--help"])).unwrap(), Invocation::Help);
        assert_eq!(
            parse_args(strings(&["list", "-h"])).unwrap(),
            Invocation::Help
        );
    }

    #[test]
    fn rejects_unknown_options_and_commands() {
        assert!(parse_args(strings(&["list", "--nope"])).is_err());
        assert!(parse_args(strings(&["wat"])).is_err());
        assert!(parse_args(strings(&[])).is_err());
    }

    #[test]
    fn parses_require_flags() {
        let invocation =
            parse_args(strings(&["require", "psr/log:^3", "--dev", "--dry-run"])).unwrap();
        let Invocation::Command(invocation) = invocation else {
            panic!("expected command");
        };
        assert_eq!(
            invocation.command,
            Command::Require {
                spec: "psr/log:^3".to_string(),
                dev: true,
                dry_run: true,
                allow_write: false,
            }
        );
    }

    #[test]
    fn validates_operand_counts() {
        assert!(parse_args(strings(&["show"])).is_err());
        assert!(parse_args(strings(&["show", "a/b", "c/d"])).is_err());
        assert!(parse_args(strings(&["run"])).is_err());
        assert!(parse_args(strings(&["list", "extra"])).is_err());
        assert!(parse_args(strings(&["remove", "a/b", "--dev"])).is_err());
    }

    #[test]
    fn package_and_spec_safety() {
        assert!(validate_package_name("psr/log").is_ok());
        assert!(validate_package_name("psr/log:^3").is_err());
        assert!(validate_package_name("-rf").is_err());
        assert!(validate_package_name("psr").is_err());
        assert!(validate_package_name("psr/ log").is_err());

        assert!(validate_require_spec("psr/log:^3").is_ok());
        assert!(validate_require_spec("psr/log").is_ok());
        assert!(validate_require_spec("psr/log:").is_err());
        assert!(validate_require_spec("--dev").is_err());
        assert!(validate_require_spec("psr/log;rm").is_err());
    }

    #[test]
    fn script_safety() {
        assert!(validate_script_name("test").is_ok());
        assert!(validate_script_name("-x").is_err());
        assert!(validate_script_name("").is_err());
        assert!(validate_script_name("a b").is_err());
    }

    #[test]
    fn direct_requirements_merge_lock_and_dev() {
        let manifest = manifest(
            r#"{
                "require": {
                    "php": "^8.0",
                    "ext-json": "*",
                    "psr/log": "^3",
                    "guzzlehttp/guzzle": "^7"
                },
                "require-dev": {
                    "psr/log": "^3",
                    "phpunit/phpunit": "^11"
                }
            }"#,
        );
        let lock = parse_lock(
            r#"{
                "packages": [
                    {"name": "psr/log", "version": "3.0.2"},
                    {"name": "guzzlehttp/guzzle", "version": "7.10.0"}
                ],
                "packages-dev": [
                    {"name": "phpunit/phpunit", "version": "11.5.0"}
                ]
            }"#,
        )
        .unwrap();

        let packages = direct_requirements(&manifest, &lock);
        assert_eq!(
            packages,
            vec![
                Package {
                    name: "guzzlehttp/guzzle".to_string(),
                    version: Some("7.10.0".to_string()),
                    constraint: "^7".to_string(),
                    dev: false,
                },
                Package {
                    name: "phpunit/phpunit".to_string(),
                    version: Some("11.5.0".to_string()),
                    constraint: "^11".to_string(),
                    dev: true,
                },
                Package {
                    name: "psr/log".to_string(),
                    version: Some("3.0.2".to_string()),
                    constraint: "^3".to_string(),
                    dev: false,
                },
            ]
        );
    }

    #[test]
    fn direct_requirements_without_lock_uses_null_version() {
        let manifest = manifest(r#"{"require":{"psr/log":"^3"}}"#);
        let packages = direct_requirements(&manifest, &LockFile::default());
        assert_eq!(
            packages,
            vec![Package {
                name: "psr/log".to_string(),
                version: None,
                constraint: "^3".to_string(),
                dev: false,
            }]
        );
    }

    #[test]
    fn lists_and_flattens_scripts() {
        let manifest =
            manifest(r#"{"scripts":{"test":"@php -r \"echo 1;\"","all":["@test","echo done"]}}"#);
        assert_eq!(
            script_list(&manifest),
            vec![
                Script {
                    name: "all".to_string(),
                    commands: strings(&["@test", "echo done"]),
                },
                Script {
                    name: "test".to_string(),
                    commands: strings(&["@php -r \"echo 1;\""]),
                },
            ]
        );
    }

    #[test]
    fn normalizes_outdated_and_adds_constraints() {
        let manifest = manifest(
            r#"{"require":{"guzzlehttp/guzzle":"^7"},"require-dev":{"phpunit/phpunit":"^11"}}"#,
        );
        let value = serde_json::json!([
            {"name":"phpunit/phpunit","version":"11.0.0","latest":"11.5.0"},
            {"name":"guzzlehttp/guzzle","version":"7.9.0","latest":"7.10.0"}
        ]);
        assert_eq!(
            normalize_outdated(&value, &manifest).unwrap(),
            vec![
                OutdatedPackage {
                    name: "guzzlehttp/guzzle".to_string(),
                    installed: "7.9.0".to_string(),
                    latest: Some("7.10.0".to_string()),
                    constraint: Some("^7".to_string()),
                },
                OutdatedPackage {
                    name: "phpunit/phpunit".to_string(),
                    installed: "11.0.0".to_string(),
                    latest: Some("11.5.0".to_string()),
                    constraint: Some("^11".to_string()),
                },
            ]
        );
    }

    #[test]
    fn accepts_outdated_installed_wrapper() {
        let value = serde_json::json!({
            "installed": [{"name":"psr/log","version":"3.0.1","latest":"3.0.2"}]
        });
        let packages = normalize_outdated(&value, &Manifest::default()).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "psr/log");
    }

    #[test]
    fn parses_validate_results() {
        let valid = parse_validate("./composer.json is valid\n", "", true);
        assert!(valid.valid);
        assert!(valid.errors.is_empty());
        assert!(valid.warnings.is_empty());

        let warnings = parse_validate(
            "./composer.json is valid, but with a few warnings\n",
            "# General warnings\n- exact version constraint\n",
            true,
        );
        assert!(warnings.valid);
        assert_eq!(warnings.warnings, strings(&["exact version constraint"]));

        let invalid = parse_validate(
            "",
            "\nIn Factory.php line 317:\n\n  \"./composer.json\" does not match the expected JSON schema:\n   - name : bad\n\nvalidate [--no-check-all]\n",
            false,
        );
        assert!(!invalid.valid);
        assert_eq!(invalid.errors.len(), 1);
        assert!(invalid.errors[0].contains("does not match"));
        assert!(invalid.errors[0].contains("name : bad"));

        let ansi = parse_validate(
            "\u{1b}[32m./composer.json is valid\u{1b}[39m\n",
            "\u{1b}[33m# General warnings\u{1b}[39m\n\u{1b}[33m- warning\u{1b}[39m\n",
            true,
        );
        assert!(ansi.valid);
        assert_eq!(ansi.warnings, strings(&["warning"]));
    }

    #[test]
    fn normalizes_show_details() {
        let value = serde_json::json!({
            "name": "psr/log",
            "description": "Common interface for logging libraries",
            "versions": ["3.0.2", "3.0.1"],
            "requires": {"php": ">=8.0.0"},
            "keywords": ["psr", "log"],
            "extra": true
        });
        assert_eq!(
            normalize_show(&value).unwrap(),
            PackageDetails {
                name: "psr/log".to_string(),
                versions: strings(&["3.0.2", "3.0.1"]),
                description: Some("Common interface for logging libraries".to_string()),
                requires: BTreeMap::from([("php".to_string(), ">=8.0.0".to_string())]),
            }
        );
        assert!(normalize_show(&serde_json::json!({})).is_err());
    }

    #[test]
    fn finds_project_by_walking_up() {
        let root = temp_dir("project");
        fs::write(root.join("composer.json"), "{}").unwrap();
        let nested = root.join("a/b");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            find_project(&nested).unwrap(),
            fs::canonicalize(&root).unwrap()
        );
        assert!(find_project(&nested.join("missing")).is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
