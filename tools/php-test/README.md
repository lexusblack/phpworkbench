# php-test

PHPUnit/Pest test-runner bridge for the IDE. It discovers tests, runs the
project's own PHPUnit or Pest binary with a JUnit log, and normalizes the
result into stable JSON.

## Usage

```text
php-test list [--dir PATH] [--json]
php-test run  [--dir PATH] [--filter PATTERN] [--file REL]
              [--group NAME] [--coverage] [--json] [--no-coverage]
```

- `--dir` defaults to `.`; the runner walks up from it to find a Composer
  `vendor/bin/pest` or `vendor/bin/phpunit`. Pest wins if both exist.
- `list` reports `*Test.php` files under `tests/`, plus directories and files
  declared inside `<testsuites>` in the nearest `phpunit.xml` or
  `phpunit.xml.dist`.
- `run` invokes the discovered binary with `--log-junit <temp>`. `--filter`,
  `--group`, and `--no-coverage` are forwarded; `--file` is passed as the
  positional test path.
- `--coverage` adds `--coverage-clover <temp>` and runs PHP with
  `XDEBUG_MODE=coverage`. When the nearest `phpunit.xml` has no `<source>`
  element and `src/` exists, `--coverage-filter src` is added so PHPUnit 10+
  emits a report. `--coverage` and `--no-coverage` are mutually exclusive.

Examples:

```sh
php-test list --dir fixtures/test-runner
php-test list --dir fixtures/test-runner --json
php-test run --dir fixtures/test-runner
php-test run --dir fixtures/test-runner --file tests/CalculatorTest.php
php-test run --dir fixtures/test-runner --filter Failing
php-test run --dir fixtures/test-runner --json
php-test run --dir fixtures/test-runner --coverage
php-test run --dir fixtures/test-runner --file tests/CalculatorTest.php --coverage --json
```

## JSON output

`run --json` emits one compact object:

```json
{
  "tool": "phpunit",
  "tests": [
    {
      "name": "testAddsNumbers",
      "class": "App\\Tests\\CalculatorTest",
      "file": "tests/CalculatorTest.php",
      "time": 0.00037,
      "status": "pass",
      "message": ""
    }
  ],
  "summary": {
    "total": 1,
    "passed": 1,
    "failed": 0,
    "errors": 0,
    "skipped": 0,
    "time": 0.00037
  },
  "coverage": {
    "lines_total": 6,
    "lines_covered": 6,
    "percent": 100.0,
    "files": [
      {"file": "src/Calculator.php", "total": 6, "covered": 6, "percent": 100.0}
    ]
  }
}
```

- `status` is `pass`, `fail`, `error`, or `skip`.
- `message` contains the normalized failure/error message or the skipped
  reason; `""` for passing tests.
- `file` is relative to `--dir` when the JUnit path is inside the project,
  otherwise the absolute path reported by the runner.
- `summary.time` is the sum of testcase times.
- `coverage` is present on every `run --json` call: `null` when coverage was
  not requested or could not be collected, otherwise totals plus one entry per
  file. `lines_total`/`lines_covered` count Clover `stmt` and `method` lines;
  `percent` is `0.0` when a file has no executable lines.

`list --json` emits:

```json
{"tool":"phpunit","files":["tests/CalculatorTest.php","tests/FailingTest.php"]}
```

Human `run` output prints one status line per test followed by a summary:

```text
PASS App\Tests\CalculatorTest::testAddsNumbers (0.000s)
FAIL App\Tests\FailingTest::testFailsOnPurpose - Failed asserting that 5 is identical to 4.
SKIP App\Tests\FailingTest::testIsSkipped (0.000s)
Tests: 5 | Passed: 3 | Failed: 1 | Errors: 0 | Skipped: 1 | Time: 0.004s
```

With `--coverage`, a coverage block follows the summary:

```text
Coverage:
src/Calculator.php: 100.00% (6/6)
Total: 100.00% (6/6)
```

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | All tests passed |
| 1 | At least one test failed or errored |
| 2 | Tool, configuration, or invocation error (missing binary, no JUnit output, bad arguments) |

## JUnit parsing

A small built-in XML parser handles the JUnit XML; no XML crate is used. It
supports entities, CDATA, nested testsuites, and `<failure>`, `<error>`, and
`<skipped>` children. For failure/error text, the leading `Class::method`
line and the stack-trace paragraph are stripped.

## Limitations

- One PHPUnit/Pest process per run; no parallel execution or sharding.
- Coverage is file-level only: no per-class, branch, path, or condition
  coverage. Clover `cond` lines are ignored.
- Coverage needs Xdebug or PCOV. If the coverage run fails or produces no
  Clover report, `php-test` prints a warning and still reports the test
  results; when the coverage run produces no JUnit at all (for example no
  driver is loaded) it retries once without coverage and preserves the exit
  code.
- The `src/` coverage-filter fallback is a convenience. Projects whose source
  lives elsewhere should declare `<source><include>` in `phpunit.xml`.
- Pest projects with both binaries always use Pest; there is no tool override.
- PHP must be available on `PATH` because Composer's bin proxies use
  `#!/usr/bin/env php`.
- Discovery only walks `tests/` and parseable `<testsuites>` entries, matching
  `*Test.php`. Custom suffixes, glob includes, and unresolved config variables
  are ignored.
- Test ordering follows the JUnit report; duplicate testcase output is not
  merged.
- Skipped reasons are shown only when the runner writes them into JUnit;
  PHPUnit currently emits an empty `<skipped/>` element.

## Development

```sh
cargo test --all-targets
cargo clippy --all-targets
cargo build --release
```

The fixture at `fixtures/test-runner` has three passing tests and one failing
plus one skipped test. Install it with `composer install` and run the release
binary against it, with PHP on `PATH`.

## Layout

- `src/lib.rs` — argument parsing, discovery, JUnit/XML parsing, rendering.
- `src/main.rs` — process execution, temp JUnit handling, exit codes.
- `tests/cli.rs` — end-to-end tests with a fake `vendor/bin/phpunit`.
