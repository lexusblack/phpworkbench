use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use php_lint::{
    analyse_path, human_exit_code, normalize, parse_args, render_human, resolve_phpstan,
    validate_dir, CheckOptions, Invocation,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(&args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("php-lint: {error:#}");
            2
        }
    };
    std::process::exit(code);
}

fn run(args: &[String]) -> Result<i32> {
    match parse_args(args.iter().cloned())? {
        Invocation::Help => {
            print!("{}", php_lint::USAGE);
            Ok(0)
        }
        Invocation::Check(options) => check(&options),
    }
}

fn check(options: &CheckOptions) -> Result<i32> {
    validate_dir(&options.dir)?;
    validate_file(options)?;
    let phpstan = resolve_phpstan(options)?;

    let mut command = Command::new(&phpstan);
    command
        .current_dir(&options.dir)
        .arg("analyse")
        .arg(analyse_path(options))
        .arg("--error-format=json")
        .arg("--no-progress")
        .arg("--no-interaction")
        .arg(format!("--memory-limit={}", options.memory))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(level) = options.level {
        command.arg("--level").arg(level.to_string());
    }

    let output = command.output().with_context(|| {
        format!(
            "failed to run PHPStan at {}.\n\
             Install it with: composer require --dev phpstan/phpstan",
            phpstan.display()
        )
    })?;

    let exit_code = output.status.code().unwrap_or(2);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let report = match normalize(&stdout, exit_code) {
        Ok(report) => report,
        Err(error) => {
            let trimmed = stderr.trim();
            if trimmed.is_empty() {
                bail!("{error}");
            }
            bail!("{error}\nPHPStan stderr:\n{trimmed}");
        }
    };

    if options.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        print!("{}", render_human(&report));
    }

    Ok(human_exit_code(&report))
}

fn validate_file(options: &CheckOptions) -> Result<()> {
    let Some(file) = &options.file else {
        return Ok(());
    };
    let candidate = if file.is_absolute() {
        file.clone()
    } else {
        options.dir.join(file)
    };
    if !candidate.is_file() {
        bail!("target file does not exist: {}", candidate.display());
    }
    Ok(())
}
