# php-lint

A small Rust CLI that runs PHPStan or Psalm and normalizes their JSON output so
an IDE can turn the result into inspections. It is intentionally a bridge, not
a linter: all analysis is performed by the project's own analyser installation.

## Build

```sh
cargo build --release
# binary: target/release/php-lint
```

## Usage

```sh
php-lint check [--dir PATH] [--file PATH] [--level N] [--json]
               [--memory 1G] [--phpstan PATH] [--tool auto|phpstan|psalm]
php-lint --lsp [--tool auto|phpstan|psalm]
```

| Option | Meaning |
| --- | --- |
| `--dir PATH` | Project directory to analyse. Defaults to `.`. |
| `--file PATH` | Analyse one file, relative to `--dir` (or absolute). |
| `--level N` | Pass `--level N` to PHPStan. Ignored by Psalm (uses `psalm.xml`). |
| `--json` | Print the normalized JSON report. |
| `--memory SIZE` | Analyser memory limit. Defaults to `1G`. |
| `--phpstan PATH` | Use an explicit PHPStan executable instead of discovery. |
| `--tool TOOL` | `auto` (default), `phpstan`, or `psalm`. |

Tool selection precedence is `--tool`, then the `PHP_LINT_TOOL` environment
variable, then `auto`. Passing `--phpstan PATH` without `--tool` selects
PHPStan. `auto` prefers `vendor/bin/phpstan` and falls back to
`vendor/bin/psalm`, walking up from `--dir`.

PHPStan is discovered by walking up from `--dir` until `vendor/bin/phpstan`
exists. It always runs with `--error-format=json --no-progress
--no-interaction` and the configured memory limit, so its agent preamble cannot
corrupt the JSON.

Psalm is discovered the same way (`vendor/bin/psalm`) and runs with
`--output-format=json --no-progress --no-cache` plus the memory limit and the
selected path or directory. `--no-cache` is supported by the tested Psalm
6.20.1 and is passed unconditionally.

Examples:

```sh
php-lint check --dir fixtures/lint
php-lint check --dir fixtures/lint --tool psalm
php-lint check --dir fixtures/lint --file src/Clean.php --level 6 --json
PHP_LINT_TOOL=psalm php-lint check --dir fixtures/lint
```

Human output is one line per message:

```
/abs/path/src/Broken.php:11: Undefined variable: $missing [variable.undefined]
```

## JSON shape

`--json` prints exactly one object:

```json
{
  "errors": [
    {
      "file": "/abs/path/src/Broken.php",
      "line": 11,
      "message": "Undefined variable: $missing",
      "identifier": "variable.undefined",
      "severity": "error"
    }
  ],
  "totals": { "files": 1, "errors": 1 },
  "exit_code": 1,
  "tool": "phpstan"
}
```

- `file` is the absolute path when the analyser reports one.
- `line` is `0` if the analyser omitted it.
- `identifier` is the PHPStan identifier or the Psalm issue `type`; `null`
  when absent.
- `severity` is `error` for every PHPStan message. For Psalm it maps `error`,
  `warning`, and `info`; unknown or missing values become `error`.
- `totals.files` counts distinct files that contain messages.
- `exit_code` is the analyser process exit code (`tool` says which one).
- `tool` is `phpstan` or `psalm`.

Psalm's JSON output is accepted both as a top-level array of issues (Psalm
6.20) and as an object with `issues`/`errors` keys. Non-empty Psalm `errors` are
treated as a fatal analyser error, mirroring PHPStan's `errors` handling.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | No errors. |
| `1` | The analyser reported at least one error. |
| `2` | Tool or configuration error: missing analyser, invalid arguments, bad config, unparsable output, missing target. |

An analyser reporting issues always makes the CLI exit `1`, even when Psalm's
JSON mode exits `0` or `2` itself. Tool errors go to stderr. Missing-tool
messages include the install command:

```
php-lint: Psalm not found: no vendor/bin/psalm in . or any parent directory.
Install it with: composer require --dev vimeo/psalm
```

## Fixture

`fixtures/lint` is a minimal PSR-4 project with PHPStan at level 6 and Psalm at
error level 6. Install and run it with:

```sh
cd fixtures/lint
composer install
../../tools/php-lint/target/release/php-lint check --dir . --tool psalm --json
```

`src/Clean.php` is clean. `src/Broken.php` and `src/PsalmBroken.php` contain an
undefined variable and a call to an undefined method; `Broken.php` also has a
bad return type. PHPStan reports 5 errors and Psalm reports 6, so both tools
exit `1`.

## LSP mode

`php-lint --lsp` speaks JSON-RPC over stdio with `Content-Length` framing. It
implements `initialize`, `initialized`, `shutdown`, and `exit`, and publishes
diagnostics on `textDocument/didOpen` and `textDocument/didSave` for `.php`
files. `didChange` is ignored.

The project is found by walking up from the file to the nearest matching
configuration: `phpstan.neon`/`phpstan.neon.dist` for PHPStan,
`psalm.xml`/`psalm.xml.dist` for Psalm, or any of them for `auto`. The tool is
chosen with the same precedence as the CLI (`--tool`, then `PHP_LINT_TOOL`, then
`auto`), and each diagnostic's `source` is `phpstan` or `psalm`. If the
analyser or its configuration is missing, no diagnostics are published and the
reason is written to stderr.

## Limitations

- No baseline support or baseline generation; use PHPStan's or Psalm's own
  baseline configuration.
- One analyser process per check, including each LSP open/save. There is no
  incremental in-memory analysis.
- No cache of our own. Psalm is invoked with `--no-cache`; PHPStan's cache is
  whatever its configuration provides.
- `--level` only applies to PHPStan. Psalm error levels come from `psalm.xml`.
- Psalm is always invoked with `--no-cache`; a hypothetical old Psalm without
  that flag is not supported.
- `--phpstan` only overrides the PHPStan executable; Psalm is always discovered
  as `vendor/bin/psalm`.
- POSIX-oriented discovery (`vendor/bin/phpstan`, `vendor/bin/psalm`); Windows
  `.bat` wrappers are not discovered.
- The analyser must be runnable directly (its shebang needs `php` on `PATH`)
  unless `--phpstan` points at a suitable wrapper.
