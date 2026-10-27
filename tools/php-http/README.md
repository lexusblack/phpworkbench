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
php-http run <file> --name <request> [--var key=value]... [--env file] [--json] [--follow] [--history <file>]
php-http run <file> --all [--var key=value]... [--env file] [--json] [--follow] [--history <file>]

php-http collection list <collection.json>
php-http collection env <collection.json> --env <environment.json> [--var key=value]...
php-http collection run <collection.json> --request <name|id> [--env <environment.json>] [--var key=value]... [--json] [--follow] [--history <file>]
php-http collection run <collection.json> --all [--env <environment.json>] [--var key=value]... [--json] [--fail-fast] [--follow] [--history <file>]
```

- `list` prints `name<TAB>method<TAB>raw-url`.
- `--env` loads simple `KEY=VALUE` lines (blank lines and `#` comments ignored).
- `--var` overrides values from the file and env file.
- `--follow` adds `curl --location`.
- `--json` prints `{name, method, url, status, headers, body}`; with `--all` it prints an array.
- `--history <file>` appends one JSON line per executed request (see below).
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
- Request bodies (see [Body modes](#body-modes)):
  - `raw` — sent verbatim with `--data-binary`. A JSON body gets
    `Content-Type: application/json`, otherwise `text/plain`, unless a content type
    header is already present.
  - `urlencoded` — built from `body.urlencoded[]` (`key`, `value`, `disabled`).
    Keys and values are percent-encoded as `application/x-www-form-urlencoded`
    (spaces become `+`); disabled entries are skipped.
  - `formdata` — sent with curl `-F` as `multipart/form-data`. `type: "text"` entries
    become text fields; `type: "file"` entries use `src` as a path relative to the
    collection file. A missing file aborts the request with a clear error. curl adds
    the multipart boundary itself, so no `Content-Type` header is injected.
  - `graphql` — sends `{"query": ..., "variables": ...}` as `application/json`.
    `variables` may be a JSON string (serialized as JSON when it parses) or a JSON
    object/array.
  - Unknown modes abort with `unsupported Postman body mode: <mode>`.
- `auth` types `bearer`, `basic`, and `apikey` (header or query), inherited from
  collection -> folder -> request; request auth wins, and `noauth` clears it.
  Auth headers/query parameters are added regardless of the body mode.
- `{{variables}}` in URLs, headers, bodies, and auth values.
- Status, response headers, body, elapsed time (ms), and body size (bytes) are captured.
  `size` is the raw response body length.

Content-Type precedence: an explicit request header always wins. Otherwise the body
mode sets it (`application/json` for JSON raw and GraphQL, `application/x-www-form-urlencoded`
for urlencoded), or curl generates the multipart boundary for formdata.

### Body modes

```json
{"mode": "urlencoded", "urlencoded": [
  {"key": "full name", "value": "Ada Lovelace"},
  {"key": "skip", "value": "x", "disabled": true}
]}

{"mode": "formdata", "formdata": [
  {"key": "title", "value": "hello", "type": "text"},
  {"key": "upload", "type": "file", "src": "upload.txt"}
]}

{"mode": "graphql", "graphql": {
  "query": "query($id: Int!) { item(id: $id) { name } }",
  "variables": "{\"id\": 7}"
}}
```

For formdata, `src` is resolved against the directory containing the collection file
(`fixtures/postman/upload.txt` above). Absolute paths are used as-is.

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

### Request history

`--history <file>` is opt-in and works for `run` and `collection run` (single request and
`--all`). Each executed request appends one JSON object per line (JSON Lines):

```json
{"timestamp":1793059657,"collection":"Demo API","request_id":"req-urlencoded","request_name":"POST urlencoded","method":"POST","url":"http://127.0.0.1:8765/echo/urlencoded","status":200,"time_ms":1,"size":245,"body":"{\n  \"form\": ..."}
```

- `timestamp` is Unix seconds (UTC).
- `collection` is the collection name for Postman runs, or the `.http` file path.
- `request_id` and `request_name` are the Postman id/name; for `.http` both are the request name.
- `body` is the first 500 characters of the response body (UTF-8 safe).
- The file is created if absent and appended to, never truncated.
- Requests that fail before execution (for example an unresolved URL variable) are not recorded.

## Examples

```sh
php-http run fixtures/http-test/api.http --name list
php-http run fixtures/http-test/api.http --all --json
php-http run fixtures/http-test/api.http --name list --var host=http://localhost:9000

php-http collection list fixtures/postman/demo.postman_collection.json
php-http collection env fixtures/postman/demo.postman_collection.json --env fixtures/postman/demo.postman_environment.json
php-http collection run fixtures/postman/demo.postman_collection.json --request req-get-items --env fixtures/postman/demo.postman_environment.json
php-http collection run fixtures/postman/demo.postman_collection.json --request req-formdata --env fixtures/postman/demo.postman_environment.json
php-http collection run fixtures/postman/demo.postman_collection.json --all --env fixtures/postman/demo.postman_environment.json --history /tmp/php-http.jsonl
php-http collection run fixtures/postman/demo.postman_collection.json --all --env fixtures/postman/demo.postman_environment.json --json
```

The `fixtures/postman/echo_server.py` helper echoes method, content type, parsed form
fields, file presence, and JSON bodies on port 8765:

```sh
python3 fixtures/postman/echo_server.py --port 8765
```

## Limitations

- No pre-request scripts, test scripts, or collection-level events.
- No OAuth1/OAuth2 flows or client certificates; only bearer/basic/apikey are supported.
- No binary streaming: request bodies are strings and response bodies are buffered,
  then decoded lossily as UTF-8 (`size` still reports raw bytes).
- No response variables, dynamic Postman variables (`$randomUUID`, etc.), or runtime
  variable mutation; only declared variables are used.
- No `HTTP/2`, gzip decoding, or streaming output.
- Relative URLs are passed to `curl` as-is (no host joining beyond variable interpolation).
- Repeated response headers are comma-joined in JSON output.
- Formdata text fields use `-F key=value`; a literal leading `@` in a text value would be
  interpreted by curl as a file reference.

