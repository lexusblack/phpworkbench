# php-composer

A small Rust CLI that exposes Composer to the IDE. It reads `composer.json` /
`composer.lock` directly for local commands and shells out to the `composer`
CLI for anything that needs Composer's dependency graph. It does not implement
dependency resolution itself.

## Build

```sh
cargo build --release
# binary: target/release/php-composer
```

Runtime requirements:

- `composer` on `PATH` for `outdated`, `validate`, `show`, `run`, `require`,
  and `remove`. `list` and `scripts` only read local files.
- PHP on `PATH` for Composer itself and for Composer scripts.

Set `PHP_COMPOSER_BIN` to use a specific Composer executable. A missing
executable produces an install hint and exit code `2`.

## Usage

```sh
php-composer [--dir PATH] <command> [options]
```

| Command | Description |
| --- | --- |
| `list [--json]` | Direct `require` / `require-dev` packages |
| `outdated [--json]` | `composer outdated --format=json --direct` |
| `validate [--json]` | `composer validate --no-check-publish --no-interaction` |
| `show <package> [--json]` | `composer show <package> --format=json` |
| `scripts [--json]` | Scripts declared in `composer.json` |
| `run <script> [--json]` | `composer run-script <script>` |
| `require <pkg>[:constraint] [--dev] [--dry-run] [--allow-write] [--json]` | Add a dependency |
| `remove <pkg> [--dry-run] [--allow-write] [--json]` | Remove a dependency |

`--dir` defaults to `.` and walks up until it finds `composer.json`. It may be
placed before or after the command. `--json` may appear anywhere.

`require` and `remove` refuse to modify files unless `--dry-run` or
`--allow-write` is present. `--dry-run` is forwarded to Composer and does not
write `composer.json` or `composer.lock`.

Examples:

```sh
php-composer --dir fixtures/composer-test list --json
php-composer outdated --json --dir fixtures/composer-test
php-composer run test --dir fixtures/composer-test
php-composer require psr/container:^2 --dry-run --json
```

## JSON shapes

`list` prints an array sorted by package name. Platform requirements (`php`,
`ext-*`, `lib-*`, `composer-*`) are skipped. `version` comes from
`composer.lock`; it is `null` when the lock file is absent or the package is not
locked.

```json
[{"name":"psr/log","version":"3.0.1","constraint":"^3","dev":false}]
```

`outdated` prints an array. `latest` and `constraint` are nullable. `latest`
is Composer's latest version (which may be a new major); it is not limited by
the declared constraint.

```json
[{"name":"psr/log","installed":"3.0.1","latest":"3.0.2","constraint":"^3"}]
```

`validate` prints one object. Composer's text output is parsed because its
`validate` command has no JSON mode. Warnings do not make `valid` false.

```json
{"valid":true,"errors":[],"warnings":[]}
```

`show` strips Composer's output to these keys; `description` is nullable and
`requires` maps package name to constraint.

```json
{"name":"psr/log","versions":["3.0.1"],"description":"Common interface for logging libraries","requires":{"php":">=8.0.0"}}
```

`scripts` prints an array sorted by script name. A string command is normalized
to a one-element array.

```json
[{"name":"test","commands":["@php -r \"echo \\\"ok\\n\\\";\""]}]
```

`run` prints captured output and the script's exit code:

```json
{"script":"test","exit_code":0,"stdout":"ok\n","stderr":"> @php ...\n"}
```

`require` / `remove` print the action, whether it was a dry run, and Composer's
exit code and streams:

```json
{"action":"require","package":"psr/container:^2","dev":false,"dry_run":true,"exit_code":0,"stdout":"...","stderr":"..."}
```

Without `--json`, output is human-readable: tab-separated package rows for
`list` / `outdated` / `scripts`, labeled fields for `show`, `valid` or
`invalid` plus `error:` / `warning:` lines for `validate`, and Composer's
streams for `run` / `require` / `remove`.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success. |
| `1` | Operation failed: invalid `composer.json`, a failed `require`/`remove`, or (for `validate`) an invalid manifest. |
| `2` | Usage, tool, or configuration error: missing Composer, no `composer.json`, unreadable/malformed JSON, bad arguments, or Composer failing for a read command. |
| `N` | `run` propagates the Composer script's exit code. |

## Safety and argument handling

- Operands are passed as separate argv entries, never through a shell.
- `--` separates options from package/script operands.
- Package names must contain `/` and only ASCII alphanumerics, `_`, `.`, `-`,
  `/`. Leading `-` is rejected.
- Script names must not start with `-` or contain whitespace.
- Unknown options and extra operands exit `2` without invoking Composer.
- Writes require `--dry-run` (preview) or `--allow-write` (commit).

## Fixture

`fixtures/composer-test` requires `psr/log ^3` and `guzzlehttp/guzzle ^7`,
defines `test`, and is installed with:

```sh
cd fixtures/composer-test
composer install
```

`list` / `outdated` / `validate` / `scripts` / `run test` / `show` and a
temporary `require --dry-run` are verified against this fixture with the real
Composer CLI.

## Limitations

- No dependency resolution UI: versions and compatibility are Composer's job.
- One Composer process per command; no daemon, cache, or long-lived session.
- `outdated` is direct-only and reports Composer's absolute latest version.
- `show` requires the package to be installed; `--available` is not used.
- `validate` parsing is text-based. Multi-line Composer exception panels are
  flattened into one error string, and root-version noise is ignored.
- `list` skips platform packages and reports `null` versions without a lock.
- Composer authentication and global config are inherited from the environment
  (`COMPOSER_AUTH`, `COMPOSER_HOME`, etc.); this tool adds no credential
  handling.
