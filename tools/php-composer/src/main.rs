use std::io::{self, ErrorKind, Write};
use std::path::Path;
use std::process::{Command as ProcessCommand, Output};

use anyhow::{bail, Context, Result};
use php_composer::{
    direct_requirements, find_project, load_lock, load_manifest, normalize_outdated,
    normalize_show, parse_args, parse_validate, script_list, Command, Invocation, Package, Script,
    USAGE,
};
use serde_json::{json, Value};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(&args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("php-composer: {error:#}");
            2
        }
    };
    std::process::exit(code);
}

fn run(args: &[String]) -> Result<i32> {
    let invocation = parse_args(args.iter().cloned())?;
    let Invocation::Command(invocation) = invocation else {
        print!("{USAGE}");
        return Ok(0);
    };

    let root = find_project(&invocation.options.dir)?;
    let json = invocation.options.json;

    match invocation.command {
        Command::List => {
            let manifest = load_manifest(&root)?;
            let lock = load_lock(&root)?;
            let packages = direct_requirements(&manifest, &lock);
            if json {
                println!("{}", serde_json::to_string(&packages)?);
            } else {
                for package in &packages {
                    print_package(package);
                }
            }
            Ok(0)
        }
        Command::Scripts => {
            let manifest = load_manifest(&root)?;
            let scripts = script_list(&manifest);
            if json {
                println!("{}", serde_json::to_string(&scripts)?);
            } else {
                for script in &scripts {
                    print_script(script);
                }
            }
            Ok(0)
        }
        Command::Outdated => {
            let manifest = load_manifest(&root)?;
            let output = run_composer(
                &root,
                &["outdated", "--format=json", "--direct", "--no-interaction"],
            )?;
            let value = outdated_json(&output)?;
            let packages = normalize_outdated(&value, &manifest)?;
            if json {
                println!("{}", serde_json::to_string(&packages)?);
            } else {
                for package in &packages {
                    println!(
                        "{}\t{} -> {}\t{}",
                        package.name,
                        package.installed,
                        package.latest.as_deref().unwrap_or("-"),
                        package.constraint.as_deref().unwrap_or("-")
                    );
                }
            }
            Ok(0)
        }
        Command::Validate => {
            let output = run_composer(
                &root,
                &["validate", "--no-check-publish", "--no-interaction"],
            )?;
            let report = parse_validate(
                &String::from_utf8_lossy(&output.stdout),
                &String::from_utf8_lossy(&output.stderr),
                output.status.success(),
            );
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                println!("{}", if report.valid { "valid" } else { "invalid" });
                for error in &report.errors {
                    println!("error: {error}");
                }
                for warning in &report.warnings {
                    println!("warning: {warning}");
                }
            }
            Ok(if report.valid { 0 } else { 1 })
        }
        Command::Show { package } => {
            let output = run_composer(
                &root,
                &[
                    "show",
                    "--format=json",
                    "--no-interaction",
                    "--",
                    package.as_str(),
                ],
            )?;
            if !output.status.success() {
                bail!("composer show failed: {}", output_message(&output));
            }
            let value: Value = serde_json::from_slice(&output.stdout)
                .context("composer show did not return JSON")?;
            let details = normalize_show(&value)?;
            if json {
                println!("{}", serde_json::to_string(&details)?);
            } else {
                println!("name: {}", details.name);
                println!("versions: {}", details.versions.join(", "));
                if let Some(description) = &details.description {
                    println!("description: {description}");
                }
                for (name, constraint) in &details.requires {
                    println!("requires.{name}: {constraint}");
                }
            }
            Ok(0)
        }
        Command::Run { script } => {
            let output = run_composer(
                &root,
                &["run-script", "--no-interaction", "--", script.as_str()],
            )?;
            let code = output.status.code().unwrap_or(1);
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "script": script,
                        "exit_code": code,
                        "stdout": String::from_utf8_lossy(&output.stdout),
                        "stderr": String::from_utf8_lossy(&output.stderr),
                    }))?
                );
            } else {
                write_output(&output)?;
            }
            Ok(code)
        }
        Command::Require {
            spec,
            dev,
            dry_run,
            allow_write,
        } => modify(&root, "require", &spec, dev, dry_run, allow_write, json),
        Command::Remove {
            package,
            dry_run,
            allow_write,
        } => modify(&root, "remove", &package, false, dry_run, allow_write, json),
    }
}

fn modify(
    root: &Path,
    action: &str,
    package: &str,
    dev: bool,
    dry_run: bool,
    allow_write: bool,
    json: bool,
) -> Result<i32> {
    if !dry_run && !allow_write {
        bail!("refusing to {action} without --dry-run; pass --dry-run or --allow-write");
    }

    let mut args: Vec<&str> = vec![action, "--no-interaction"];
    if dev {
        args.push("--dev");
    }
    if dry_run {
        args.push("--dry-run");
    }
    args.push("--");
    args.push(package);

    let output = run_composer(root, &args)?;
    let code = output.status.code().unwrap_or(1);
    let success = output.status.success();

    if json {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "action": action,
                "package": package,
                "dev": dev,
                "dry_run": dry_run,
                "exit_code": code,
                "stdout": String::from_utf8_lossy(&output.stdout),
                "stderr": String::from_utf8_lossy(&output.stderr),
            }))?
        );
    } else {
        write_output(&output)?;
    }

    Ok(if success { 0 } else { 1 })
}

fn print_package(package: &Package) {
    let version = package.version.as_deref().unwrap_or("-");
    let constraint = if package.constraint.is_empty() {
        "-"
    } else {
        package.constraint.as_str()
    };
    if package.dev {
        println!("{}\t{}\t{}\tdev", package.name, version, constraint);
    } else {
        println!("{}\t{}\t{}", package.name, version, constraint);
    }
}

fn print_script(script: &Script) {
    println!("{}\t{}", script.name, script.commands.join(" ; "));
}

fn write_output(output: &Output) -> Result<()> {
    io::stdout().write_all(&output.stdout)?;
    io::stderr().write_all(&output.stderr)?;
    Ok(())
}

fn outdated_json(output: &Output) -> Result<Value> {
    match serde_json::from_slice(&output.stdout) {
        Ok(value) => Ok(value),
        Err(error) => {
            if !output.status.success() {
                bail!("composer outdated failed: {}", output_message(output));
            }
            Err(error).context("composer outdated did not return JSON")
        }
    }
}

fn output_message(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = stderr.trim();
    let stdout = stdout.trim();
    if !stderr.is_empty() {
        stderr.to_string()
    } else if !stdout.is_empty() {
        stdout.to_string()
    } else {
        format!("exit status {}", output.status)
    }
}

fn run_composer(root: &Path, args: &[&str]) -> Result<Output> {
    let binary = std::env::var_os("PHP_COMPOSER_BIN").unwrap_or_else(|| "composer".into());

    let output = ProcessCommand::new(&binary)
        .current_dir(root)
        .args(args)
        .env("COMPOSER_NO_INTERACTION", "1")
        .env("NO_COLOR", "1")
        .output();

    match output {
        Ok(output) => Ok(output),
        Err(error) if error.kind() == ErrorKind::NotFound => bail!(
            "composer executable not found: {}\n\
             Install Composer and make sure it is on PATH: https://getcomposer.org/download/\n\
             macOS: brew install composer\n\
             Or set PHP_COMPOSER_BIN to the composer executable.",
            binary.to_string_lossy()
        ),
        Err(error) => Err(error)
            .with_context(|| format!("failed to run composer ({})", binary.to_string_lossy())),
    }
}
