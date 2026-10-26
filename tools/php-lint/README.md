# php-lint

A small Rust CLI that runs PHPStan and normalizes its JSON output so an IDE can
turn the result into inspections. It is intentionally a bridge, not a linter:
all analysis is performed by the project's own PHPStan installation.

## Build

```sh
cargo build --release
# binary: target/release/php-lint
```

## Usage

```sh
php-lint check [--dir PATH] [--file PATH] [--level N] [--json]
               [--memory 1G] [--phpstan PATH]
php-lint --lsp
```

| Option | Meaning |
| --- | --- |
| `--dir PATH` | Project directory to analyse. Defaults to `.`. |
| `--file PATH` | Analyse one file, relative to `--dir` (or absolute). |
| `--level N` | Pass `--level N` to PHPStan. Without it, the level from `phpstan.neon` is used. |
| `--json` | Print the normalized JSON report. |
| `--memory SIZE` | PHPStan memory limit. Defaults to `1G`. |
| `--phpstan PATH` | Use an explicit PHPStan executable instead of discovery. |

PHPStan is discovered by walking up from `--dir` until `vendor/bin/phpstan`
exists. PHPStan always runs with `--error-format=json --no-progress
--no-interaction`, so its agent preamble cannot corrupt the JSON.

Example:

```sh
php-lint check --dir fixtures/lint
php-lint check --dir fixtures/lint --file src/Clean.php --level 6 --json
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
      "identifier": "variable.undefined"
    }
  ],
  "totals": { "files": 1, "errors": 1 },
  "exit_code": 1
}
```

- `file` is the path reported by PHPStan (normally absolute).
- `line` is `0` if PHPStan omitted it.
- `identifier` is `null` when PHPStan has no identifier.
- `totals.files` counts files that actually contain messages; PHPStan's JSON
  only includes such files.
- `exit_code` is PHPStan's process exit code.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | No errors. |
| `1` | PHPStan reported at least one error. |
| `2` | Tool or configuration error: missing PHPStan, invalid arguments, bad config, unparsable output, missing target. |

Tool errors go to stderr. A missing PHPStan message includes the install
command:

```
php-lint: PHPStan not found at /nonexistent.
Install it with: composer require --dev phpstan/phpstan
```

## Fixture

`fixtures/lint` is a minimal PSR-4 project configured at level 6. Install and
run it with:

```sh
cd fixtures/lint
composer install
../../tools/php-lint/target/release/php-lint check --dir . --json
```

`src/Clean.php` is clean. `src/Broken.php` contains an undefined variable, a
wrong return type, and a call to an undefined method, so the fixture exits `1`
with three errors.

## LSP mode

`php-lint --lsp` speaks JSON-RPC over stdio with `Content-Length` framing. It
implements `initialize`, `initialized`, `shutdown`, and `exit`, and publishes
PHPStan diagnostics on `textDocument/didOpen` and `textDocument/didSave` for
`.php` files. `didChange` is ignored. The project is found by walking up from
the file to the nearest `phpstan.neon` or `phpstan.neon.dist`. If PHPStan or
its configuration is missing, no diagnostics are published and the reason is
written to stderr.

## Limitations

- No baseline support or baseline generation; PHPStan's own baseline must be
  configured in `phpstan.neon`.
- One PHPStan process per check, including each LSP open/save. There is no
  incremental in-memory analysis.
- No incremental cache of our own; PHPStan's result cache is whatever its
  configuration provides.
- POSIX-oriented discovery (`vendor/bin/phpstan`); Windows `.bat` wrappers are
  not discovered.
- PHPStan must be runnable directly (its shebang needs `php` on `PATH`) unless
  `--phpstan` points at a suitable wrapper.
