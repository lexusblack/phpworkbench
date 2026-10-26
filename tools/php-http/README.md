# php-http

A small standalone Rust CLI that runs PhpStorm-style `.http` files by shelling out to `curl`.

## Build

```sh
cargo build --release
# binary: target/release/php-http
```

Requires `curl` on `PATH` (macOS ships one).

## `.http` format

```http
@host = http://127.0.0.1:8080
@token = secret

### list items
GET {{host}}/items
Authorization: Bearer {{token}}

### create item
POST {{host}}/items HTTP/1.1
Content-Type: application/json

{"name":"demo"}
```

Supported:

- Requests separated by `###` or `### name`.
- `@name = value` variables, interpolated as `{{name}}` (recursively, up to 10 passes).
- Request line `METHOD URL [HTTP/x.x]`; method defaults to `GET` and is case-normalized.
- `Name: value` headers.
- Optional body after a blank line (sent with `--data-binary`).
- Full-line comments starting with `#` or `//` ignored outside the body.

Request names must be unique for `--name` lookup. A section without a `### name` gets `METHOD-N`.

## Commands

```sh
php-http list <file>
php-http run <file> --name <request> [--var key=value]... [--env file] [--json] [--follow]
php-http run <file> --all [--var key=value]... [--env file] [--json] [--follow]
```

- `list` prints `name<TAB>method<TAB>raw-url`.
- `--env` loads simple `KEY=VALUE` lines (blank lines and `#` comments ignored).
- `--var` overrides values from the file and env file.
- `--follow` adds `curl --location`.
- `--json` prints `{name, method, url, status, headers, body}`; with `--all` it prints an array.
- Every request uses `--max-time 30`; HTTP 4xx/5xx are not CLI errors.
- An unresolved `{{variable}}` in a URL aborts with a clear error instead of being sent to curl.

## Example

```sh
php-http run fixtures/http-test/api.http --name list
php-http run fixtures/http-test/api.http --all --json
php-http run fixtures/http-test/api.http --name list --var host=http://localhost:9000
```

## Limitations

- No request-variable scoping, response variables, scripts, multipart, file uploads, or client certificates.
- No `HTTP/2`, gzip decoding, or streaming; output is buffered.
- Relative URLs are passed to `curl` as-is (no host joining beyond variable interpolation).
- Repeated response headers are comma-joined in JSON output.
- `--all` stops at the first execution error.
- Binary bodies are lossily decoded as UTF-8.
