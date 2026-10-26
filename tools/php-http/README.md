# php-http

A small standalone Rust CLI that runs PhpStorm-style `.http` files and Postman Collection v2.1 files by shelling out to `curl`.

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

php-http collection list <collection.json>
php-http collection env <collection.json> --env <environment.json> [--var key=value]...
php-http collection run <collection.json> --request <name|id> [--env <environment.json>] [--var key=value]... [--json] [--follow]
php-http collection run <collection.json> --all [--env <environment.json>] [--var key=value]... [--json] [--fail-fast] [--follow]
```

- `list` prints `name<TAB>method<TAB>raw-url`.
- `--env` loads simple `KEY=VALUE` lines (blank lines and `#` comments ignored).
- `--var` overrides values from the file and env file.
- `--follow` adds `curl --location`.
- `--json` prints `{name, method, url, status, headers, body}`; with `--all` it prints an array.
- Every request uses `--max-time 30`; HTTP 4xx/5xx are not CLI errors.
- An unresolved `{{variable}}` in a URL aborts with a clear error instead of being sent to curl.

## Postman Collection v2.1

`collection list` flattens nested folders and prints `id<TAB>name<TAB>folder<TAB>method<TAB>url`.
Top-level requests use `-` as the folder. Requests are matched by id first, then by unique name.

`collection env` prints the resolved collection/environment variables as sorted `key=value`
lines. Variables are expanded recursively, so `base={{host}}/api` resolves through `host`.

Supported request features:

- Nested folders; folder and request `variable` arrays are inherited.
- `request.url` as a raw string or object (`raw`, `host`, `path`, `query`).
- Enabled query parameters are rebuilt from `query`; `disabled: true` entries are skipped.
- Enabled request headers; disabled headers are skipped.
- `body.mode = "raw"` (raw JSON bodies work). A JSON body gets `Content-Type: application/json`
  when no content type header is present.
- `auth` types `bearer`, `basic`, and `apikey` (header or query), inherited from
  collection -> folder -> request; request auth wins, and `noauth` clears it.
- `{{variables}}` in URLs, headers, bodies, and auth values.
- Status, response headers, body, elapsed time (ms), and body size (bytes) are captured.
  `size` is the raw response body length.

Variable precedence (lowest to highest):

1. Collection variables
2. Folder variables
3. Request variables
4. Environment variables
5. `--var key=value`

This matches Postman's rule that environment values override collection values, while `--var`
acts as a local CLI override. Collection/folder/request variables are best-effort because
Postman scripts can change values at runtime.

`--request <name|id>` runs one request. `--all` runs sequentially and prints a per-request
summary line plus a total. HTTP 4xx/5xx responses are reported as `fail` but do not make the
CLI exit non-zero. `--fail-fast` stops after the first failed request (HTTP status outside
2xx-3xx) and exits non-zero. `--json` prints one object for `--request`, or
`{collection, results, summary}` for `--all`.

Postman environment files use the standard `{"values": [...]}` shape; `enabled: false` entries
are ignored. Plain `KEY=VALUE` lines are also accepted as a fallback.

## Examples

```sh
php-http run fixtures/http-test/api.http --name list
php-http run fixtures/http-test/api.http --all --json
php-http run fixtures/http-test/api.http --name list --var host=http://localhost:9000

php-http collection list fixtures/postman/demo.postman_collection.json
php-http collection env fixtures/postman/demo.postman_collection.json --env fixtures/postman/demo.postman_environment.json
php-http collection run fixtures/postman/demo.postman_collection.json --request req-get-items --env fixtures/postman/demo.postman_environment.json
php-http collection run fixtures/postman/demo.postman_collection.json --all --env fixtures/postman/demo.postman_environment.json --json
```

## Limitations

- No Postman scripts, pre-request scripts, or test scripts.
- No file uploads, multipart/form-data, urlencoded bodies, OAuth1/OAuth2 flows, or client certificates.
- No collection-level events, response variables, or dynamic Postman variables (`$randomUUID`, etc.).
- Variables set by scripts and runtime mutations are not simulated; only declared variables are used.
- No `HTTP/2`, gzip decoding, or streaming; output is buffered.
- Relative URLs are passed to `curl` as-is (no host joining beyond variable interpolation).
- Repeated response headers are comma-joined in JSON output.
- Binary bodies are lossily decoded as UTF-8; `size` still reports raw bytes.

