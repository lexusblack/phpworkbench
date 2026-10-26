use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

const OSV_BATCH: &str = "https://api.osv.dev/v1/querybatch";
const OSV_VULN: &str = "https://api.osv.dev/v1/vulns";
const PACKAGIST: &str = "https://repo.packagist.org/p2";
const NPM: &str = "https://registry.npmjs.org";
const MAX_DETAIL_FETCHES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Ecosystem {
    Composer,
    Npm,
}

impl Ecosystem {
    fn key(self) -> &'static str {
        match self {
            Self::Composer => "composer",
            Self::Npm => "npm",
        }
    }

    fn osv(self) -> &'static str {
        match self {
            Self::Composer => "Packagist",
            Self::Npm => "npm",
        }
    }
}

#[derive(Debug, Clone)]
struct Package {
    ecosystem: Ecosystem,
    name: String,
    version: Option<String>,
    declaration: Option<String>,
}

#[derive(Debug, Clone)]
struct Advisory {
    id: String,
    aliases: Vec<String>,
    severity: Option<String>,
    summary: Option<String>,
}

#[derive(Debug)]
struct Row {
    package: Package,
    latest: Option<String>,
    outdated: bool,
    advisories: Vec<Advisory>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(2);
    }
}

fn run() -> Result<()> {
    let mut dir = PathBuf::from(".");
    let mut json_output = false;
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => {
                dir = PathBuf::from(args.next().context("--dir requires a path")?);
            }
            "--json" => json_output = true,
            "-h" | "--help" => {
                println!(
                    "deps-audit [--dir PATH] [--json]\n\n\
                     Reads composer.lock/composer.json and package-lock.json/package.json,\n\
                     checks OSV advisories and reports outdated packages."
                );
                return Ok(());
            }
            other => bail!("unknown argument: {other}"),
        }
    }

    let packages = collect_packages(&dir)?;
    if packages.is_empty() {
        bail!(
            "no composer.lock, composer.json, package-lock.json or package.json in {}",
            dir.display()
        );
    }

    let advisories = query_advisories(&packages)?;
    let latest = query_latest(&packages);

    let mut rows = Vec::with_capacity(packages.len());
    for package in packages {
        let latest_version = latest
            .get(&(package.ecosystem, package.name.clone()))
            .cloned()
            .flatten();
        let outdated = match (&package.version, &latest_version) {
            (Some(installed), Some(latest)) => match (parse_version(installed), parse_version(latest))
            {
                (Some(installed), Some(latest)) => latest > installed,
                _ => false,
            },
            _ => false,
        };
        let package_advisories = advisories
            .get(&(package.ecosystem, package.name.clone()))
            .cloned()
            .unwrap_or_default();
        rows.push(Row {
            package,
            latest: latest_version,
            outdated,
            advisories: package_advisories,
        });
    }

    if json_output {
        print_json(&rows)?;
    } else {
        print_table(&rows);
    }

    let total_advisories: usize = rows.iter().map(|row| row.advisories.len()).sum();
    if total_advisories > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn collect_packages(dir: &Path) -> Result<Vec<Package>> {
    let mut packages = Vec::new();

    let composer_lock = dir.join("composer.lock");
    if composer_lock.exists() {
        let lock = read_json(&composer_lock)?;
        for section in ["packages", "packages-dev"] {
            if let Some(items) = lock.get(section).and_then(Value::as_array) {
                for item in items {
                    let Some(name) = item.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    packages.push(Package {
                        ecosystem: Ecosystem::Composer,
                        name: name.to_string(),
                        version: item
                            .get("version")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        declaration: None,
                    });
                }
            }
        }
    } else if dir.join("composer.json").exists() {
        let manifest = read_json(&dir.join("composer.json"))?;
        for section in ["require", "require-dev"] {
            if let Some(map) = manifest.get(section).and_then(Value::as_object) {
                for (name, constraint) in map {
                    if name == "php" || name.starts_with("ext-") || name.starts_with("lib-") {
                        continue;
                    }
                    packages.push(Package {
                        ecosystem: Ecosystem::Composer,
                        name: name.clone(),
                        version: None,
                        declaration: constraint.as_str().map(str::to_string),
                    });
                }
            }
        }
    }

    let package_lock = dir.join("package-lock.json");
    if package_lock.exists() {
        let lock = read_json(&package_lock)?;
        if let Some(objects) = lock.get("packages").and_then(Value::as_object) {
            for (path, object) in objects {
                let Some(name) = npm_name_from_lock_path(path) else {
                    continue;
                };
                if object
                    .get("link")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    continue;
                }
                let Some(version) = object.get("version").and_then(Value::as_str) else {
                    continue;
                };
                packages.push(Package {
                    ecosystem: Ecosystem::Npm,
                    name,
                    version: Some(version.to_string()),
                    declaration: None,
                });
            }
        } else if let Some(dependencies) = lock.get("dependencies").and_then(Value::as_object) {
            collect_npm_v1(dependencies, &mut packages, 0);
        }
    } else if dir.join("package.json").exists() {
        let manifest = read_json(&dir.join("package.json"))?;
        for section in ["dependencies", "devDependencies"] {
            if let Some(map) = manifest.get(section).and_then(Value::as_object) {
                for (name, constraint) in map {
                    packages.push(Package {
                        ecosystem: Ecosystem::Npm,
                        name: name.clone(),
                        version: None,
                        declaration: constraint.as_str().map(str::to_string),
                    });
                }
            }
        }
    }

    packages.sort_by(|left, right| {
        (left.ecosystem, left.name.as_str()).cmp(&(right.ecosystem, right.name.as_str()))
    });
    packages.dedup_by(|left, right| {
        left.ecosystem == right.ecosystem && left.name == right.name
    });
    Ok(packages)
}

fn collect_npm_v1(
    dependencies: &serde_json::Map<String, Value>,
    packages: &mut Vec<Package>,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    for (name, value) in dependencies {
        if let Some(version) = value.get("version").and_then(Value::as_str) {
            packages.push(Package {
                ecosystem: Ecosystem::Npm,
                name: name.clone(),
                version: Some(version.to_string()),
                declaration: None,
            });
        }
        if let Some(nested) = value.get("dependencies").and_then(Value::as_object) {
            collect_npm_v1(nested, packages, depth + 1);
        }
    }
}

fn npm_name_from_lock_path(path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let marker = "node_modules/";
    let index = path.rfind(marker)?;
    let name = &path[index + marker.len()..];
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn query_advisories(
    packages: &[Package],
) -> Result<HashMap<(Ecosystem, String), Vec<Advisory>>> {
    let mut queries = Vec::new();
    let mut owners = Vec::new();
    for (index, package) in packages.iter().enumerate() {
        let Some(version) = &package.version else {
            continue;
        };
        queries.push(json!({
            "package": {
                "name": package.name,
                "ecosystem": package.ecosystem.osv(),
            },
            "version": normalize_version(version),
        }));
        owners.push(index);
    }

    let mut ids_per_package: Vec<Vec<String>> = vec![Vec::new(); packages.len()];
    let mut all_ids = BTreeSet::new();

    for (query_chunk, owner_chunk) in queries.chunks(100).zip(owners.chunks(100)) {
        let payload = json!({ "queries": query_chunk });
        let response = http_json("POST", OSV_BATCH, Some(&payload.to_string()))?;
        let results = response
            .get("results")
            .and_then(Value::as_array)
            .context("unexpected OSV response")?;

        for (result, &package_index) in results.iter().zip(owner_chunk) {
            if let Some(vulns) = result.get("vulns").and_then(Value::as_array) {
                for vuln in vulns {
                    if let Some(id) = vuln.get("id").and_then(Value::as_str) {
                        ids_per_package[package_index].push(id.to_string());
                        all_ids.insert(id.to_string());
                    }
                }
            }
        }
    }

    let mut details: HashMap<String, Advisory> = HashMap::new();
    for id in all_ids.iter().take(MAX_DETAIL_FETCHES) {
        let url = format!("{OSV_VULN}/{id}");
        let Ok(vuln) = http_json("GET", &url, None) else {
            continue;
        };
        let aliases = vuln
            .get("aliases")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let severity = vuln
            .get("database_specific")
            .and_then(|specific| specific.get("severity"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let summary = vuln
            .get("summary")
            .and_then(Value::as_str)
            .map(str::to_string);
        details.insert(
            id.to_string(),
            Advisory {
                id: id.to_string(),
                aliases,
                severity,
                summary,
            },
        );
    }

    let mut result = HashMap::new();
    for (index, ids) in ids_per_package.into_iter().enumerate() {
        if ids.is_empty() {
            continue;
        }
        let list = ids
            .into_iter()
            .map(|id| {
                details.get(&id).cloned().unwrap_or(Advisory {
                    id,
                    aliases: Vec::new(),
                    severity: None,
                    summary: None,
                })
            })
            .collect();
        result.insert(
            (packages[index].ecosystem, packages[index].name.clone()),
            list,
        );
    }

    Ok(result)
}

fn query_latest(packages: &[Package]) -> HashMap<(Ecosystem, String), Option<String>> {
    let mut latest = HashMap::new();
    for package in packages {
        let key = (package.ecosystem, package.name.clone());
        if latest.contains_key(&key) {
            continue;
        }
        let version = match package.ecosystem {
            Ecosystem::Composer => fetch_packagist_latest(&package.name),
            Ecosystem::Npm => fetch_npm_latest(&package.name),
        };
        latest.insert(key, version.ok().flatten());
    }
    latest
}

fn fetch_packagist_latest(name: &str) -> Result<Option<String>> {
    let url = format!("{PACKAGIST}/{name}.json");
    let response = http_json("GET", &url, None)?;
    let Some(versions) = response
        .get("packages")
        .and_then(|packages| packages.get(name))
        .and_then(Value::as_array)
    else {
        return Ok(None);
    };
    for entry in versions {
        let Some(version) = entry.get("version").and_then(Value::as_str) else {
            continue;
        };
        if version.contains("dev") {
            continue;
        }
        return Ok(Some(version.to_string()));
    }
    Ok(None)
}

fn fetch_npm_latest(name: &str) -> Result<Option<String>> {
    let encoded = name.replace('/', "%2F");
    let url = format!("{NPM}/{encoded}/latest");
    let response = http_json("GET", &url, None)?;
    Ok(response
        .get("version")
        .and_then(Value::as_str)
        .map(str::to_string))
}

fn http_json(method: &str, url: &str, body: Option<&str>) -> Result<Value> {
    let mut command = Command::new("curl");
    command.args(["-fsS", "--max-time", "30", "-L", "-X", method, url]);
    if body.is_some() {
        command.args(["-H", "content-type: application/json", "--data-binary", "@-"]);
    }
    let mut child = command
        .stdin(if body.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to run curl")?;

    if let Some(body) = body {
        child
            .stdin
            .as_mut()
            .context("curl stdin unavailable")?
            .write_all(body.as_bytes())?;
    }

    let output = child.wait_with_output().context("curl failed")?;
    if !output.status.success() {
        bail!(
            "curl {url} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).with_context(|| format!("invalid JSON from {url}"))
}

fn read_json(path: &Path) -> Result<Value> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse {}", path.display()))
}

fn normalize_version(raw: &str) -> String {
    raw.trim()
        .trim_start_matches(|character| character == 'v' || character == 'V')
        .trim()
        .to_string()
}

fn parse_version(raw: &str) -> Option<semver::Version> {
    semver::Version::parse(&normalize_version(raw)).ok()
}

fn print_table(rows: &[Row]) {
    let headers = ["ECO", "PACKAGE", "INSTALLED", "LATEST", "UPD", "VULNS", "TOP"];
    let mut widths = headers.map(str::len);

    let cells: Vec<[String; 7]> = rows
        .iter()
        .map(|row| {
            let installed = row
                .package
                .version
                .clone()
                .or_else(|| row.package.declaration.clone())
                .unwrap_or_else(|| "-".to_string());
            let top = row
                .advisories
                .first()
                .map(|advisory| {
                    let mut label = String::new();
                    if let Some(severity) = &advisory.severity {
                        label.push_str(severity);
                        label.push(' ');
                    }
                    label.push_str(&advisory.id);
                    if let Some(cve) = advisory
                        .aliases
                        .iter()
                        .find(|alias| alias.starts_with("CVE-"))
                    {
                        label.push_str(&format!(" ({cve})"));
                    }
                    label
                })
                .unwrap_or_default();
            [
                row.package.ecosystem.key().to_string(),
                row.package.name.clone(),
                installed,
                row.latest.clone().unwrap_or_else(|| "-".to_string()),
                if row.outdated { "yes" } else { "no" }.to_string(),
                row.advisories.len().to_string(),
                top,
            ]
        })
        .collect();

    for cell in &cells {
        for index in 0..7 {
            widths[index] = widths[index].max(cell[index].len());
        }
    }

    let print_row = |values: &[String; 7]| {
        let mut line = String::new();
        for index in 0..7 {
            if index > 0 {
                line.push_str("  ");
            }
            line.push_str(&format!(
                "{:<width$}",
                values[index],
                width = widths[index]
            ));
        }
        println!("{}", line.trim_end());
    };

    print_row(&headers.map(str::to_string));
    print_row(&widths.map(|width| "-".repeat(width)));
    for cell in &cells {
        print_row(cell);
    }

    let outdated = rows.iter().filter(|row| row.outdated).count();
    let advisories: usize = rows.iter().map(|row| row.advisories.len()).sum();
    println!(
        "\n{} packages, {} outdated, {} advisories",
        rows.len(),
        outdated,
        advisories
    );
}

fn print_json(rows: &[Row]) -> Result<()> {
    let packages: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "ecosystem": row.package.ecosystem.key(),
                "name": row.package.name,
                "installed": row.package.version,
                "declared": row.package.declaration,
                "latest": row.latest,
                "outdated": row.outdated,
                "advisories": row.advisories.iter().map(|advisory| json!({
                    "id": advisory.id,
                    "aliases": advisory.aliases,
                    "severity": advisory.severity,
                    "summary": advisory.summary,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();

    let outdated = rows.iter().filter(|row| row.outdated).count();
    let advisories: usize = rows.iter().map(|row| row.advisories.len()).sum();
    let report = json!({
        "packages": packages,
        "summary": {
            "packages": rows.len(),
            "outdated": outdated,
            "advisories": advisories,
        },
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
