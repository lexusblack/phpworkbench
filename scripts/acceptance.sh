#!/usr/bin/env bash
#
# End-to-end acceptance checks for the existing engines.
#
# Usage: scripts/acceptance.sh [--offline]
#
# --offline skips the deps-audit check (the only check that needs network).

set -uo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OFFLINE=0

for arg in "$@"; do
  case "$arg" in
    --offline) OFFLINE=1 ;;
    -h|--help)
      sed -n '2,9p' "$0"
      exit 0
      ;;
    *)
      printf 'unknown argument: %s\n' "$arg" >&2
      exit 2
      ;;
  esac
done

# Make the locally built CLIs available even when ~/.cargo/bin is not on PATH.
PATH="$ROOT/tools/php-db/target/debug:$ROOT/tools/php-http/target/debug:$ROOT/tools/php-deploy/target/debug:$ROOT/tools/deps-audit/target/debug:/opt/homebrew/bin:$HOME/.cargo/bin:$PATH"
export PATH

cd "$ROOT" || exit 2

TMP=$(mktemp -d "${TMPDIR:-/tmp}/phpworkbench-acceptance.XXXXXX")
SERVER_PID=""
PASS=0
FAIL=0
SKIP=0
FAILURES=()

cleanup() {
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
    SERVER_PID=""
  fi
  rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

pass() {
  printf 'PASS  %s\n' "$1"
  PASS=$((PASS + 1))
}

fail() {
  printf 'FAIL  %s: %s\n' "$1" "$2"
  FAIL=$((FAIL + 1))
  FAILURES+=("$1: $2")
}

skip() {
  printf 'SKIP  %s: %s\n' "$1" "$2"
  SKIP=$((SKIP + 1))
}

json_valid() {
  python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$1" >/dev/null 2>&1
}

# ---------------------------------------------------------------- JSON files
shopt -s nullglob
JSON_FILES=(
  profile/settings.json
  profile/tasks.json
  profile/themes/phpstorm-dark.json
  fixtures/*/.zed/*.json
)
shopt -u nullglob

bad_json=()
for file in "${JSON_FILES[@]}"; do
  json_valid "$file" || bad_json+=("$file")
done

if [ ${#bad_json[@]} -eq 0 ]; then
  pass "JSON validity (${#JSON_FILES[@]} files)"
else
  fail "JSON validity" "invalid: ${bad_json[*]}"
fi

# ---------------------------------------------------------------- deps-audit
if [ "$OFFLINE" -eq 1 ]; then
  skip "deps-audit fixture" "--offline"
else
  deps_out=$(deps-audit --dir "$ROOT/fixtures/deps" 2>&1)
  deps_rc=$?
  if { [ "$deps_rc" -eq 0 ] || [ "$deps_rc" -eq 1 ]; } &&
     [[ "$deps_out" == *advisories* ]]; then
    pass "deps-audit fixture (exit $deps_rc, advisories reported)"
  else
    fail "deps-audit fixture" "exit $deps_rc, expected 'advisories' in output"
  fi
fi

# ---------------------------------------------------------------- php-db
db_dir="$ROOT/fixtures/db-test"

list_out=$(cd "$db_dir" && php-db list 2>&1)
list_rc=$?
if [ "$list_rc" -eq 0 ] &&
   [[ "$list_out" == *app* ]] &&
   [[ "$list_out" == *sqlite* ]]; then
  pass "php-db list"
else
  fail "php-db list" "exit $list_rc, expected app/sqlite in output"
fi

schema_out=$(cd "$db_dir" && php-db schema app --json 2>&1)
schema_rc=$?
if [ "$schema_rc" -eq 0 ] &&
   printf '%s' "$schema_out" | python3 -c '
import json, sys
data = json.load(sys.stdin)
tables = [t.get("name") for t in data.get("tables", [])]
sys.exit(0 if "users" in tables else 1)
' >/dev/null 2>&1; then
  pass "php-db schema app --json"
else
  fail "php-db schema app --json" "exit $schema_rc or missing 'users' table"
fi

query_file="$TMP/rows.json"
(cd "$db_dir" && php-db query app "select id, email from users order by id" --json) >"$query_file" 2>&1
query_rc=$?
if [ "$query_rc" -eq 0 ] &&
   python3 -c '
import json, sys
rows = json.load(open(sys.argv[1]))
ok = isinstance(rows, list) and len(rows) == 3 and all("id" in r and "email" in r for r in rows)
sys.exit(0 if ok else 1)
' "$query_file" >/dev/null 2>&1; then
  pass "php-db query (3 rows)"
else
  fail "php-db query" "exit $query_rc or expected 3 rows with id/email"
fi

delete_out=$(cd "$db_dir" && php-db query app "delete from users" 2>&1)
delete_rc=$?
rows_after=$(cd "$db_dir" && php-db query app "select id from users" --json 2>/dev/null)
rows_after_rc=$?
if [ "$delete_rc" -ne 0 ] &&
   [[ "$delete_out" == *refusing* ]] &&
   [ "$rows_after_rc" -eq 0 ] &&
   printf '%s' "$rows_after" | python3 -c '
import json, sys
sys.exit(0 if len(json.load(sys.stdin)) == 3 else 1)
' >/dev/null 2>&1; then
  pass "php-db delete refused"
else
  fail "php-db delete refused" "exit $delete_rc (expected refusal) or rows changed"
fi

# ---------------------------------------------------------------- php-http
http_file="$ROOT/fixtures/http-test/api.http"

http_list=$(php-http list "$http_file" 2>&1)
http_list_rc=$?
http_count=$(printf '%s\n' "$http_list" | awk 'NF { n++ } END { print n + 0 }')
if [ "$http_list_rc" -eq 0 ] && [ "$http_count" -eq 3 ]; then
  pass "php-http list (3 requests)"
else
  fail "php-http list" "exit $http_list_rc, parsed $http_count requests (expected 3)"
fi

cat >"$TMP/http_server.py" <<'PY'
import json
from http.server import BaseHTTPRequestHandler, HTTPServer


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({"ok": True, "path": self.path}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


HTTPServer(("127.0.0.1", 8765), Handler).serve_forever()
PY

python3 "$TMP/http_server.py" >"$TMP/http_server.log" 2>&1 &
SERVER_PID=$!

server_ready=0
for _ in $(seq 1 50); do
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    break
  fi
  if python3 -c '
import socket, sys
s = socket.socket()
s.settimeout(0.2)
try:
    s.connect(("127.0.0.1", 8765))
except OSError:
    sys.exit(1)
finally:
    s.close()
' >/dev/null 2>&1; then
    server_ready=1
    break
  fi
  sleep 0.1
done

if [ "$server_ready" -ne 1 ]; then
  fail "php-http run" "temporary server on 127.0.0.1:8765 did not start (port busy?)"
else
  run_out=$(php-http run "$http_file" --name list 2>&1)
  run_rc=$?
  if [ "$run_rc" -eq 0 ] && [[ "$run_out" == *"HTTP 200"* ]]; then
    pass "php-http run list (HTTP 200)"
  else
    fail "php-http run" "exit $run_rc, expected 'HTTP 200'"
  fi
fi

if [ -n "$SERVER_PID" ]; then
  kill "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=""
fi

# ---------------------------------------------------------------- php-deploy
deploy_dir="$ROOT/fixtures/deploy-test"
deploy_target="$TMP/deploy-target"
deploy_config="$TMP/deploy.json"
mkdir -p "$deploy_target"

cat >"$deploy_config" <<EOF
{
  "default": "local",
  "servers": [
    {
      "name": "local",
      "protocol": "local",
      "local_root": "$deploy_dir",
      "remote_root": "$deploy_target",
      "exclude": ["vendor/", "node_modules/", ".git/", ".zed/", "*.log"],
      "upload_on_save": false
    }
  ]
}
EOF

deploy_out=$(cd "$deploy_dir" && php-deploy --config "$deploy_config" sync 2>&1)
deploy_rc=$?
forbidden=$(find "$deploy_target" \
  \( -path '*/vendor/*' -o -path '*/.zed/*' -o -name '*.log' \) \
  -print -quit 2>/dev/null)

if [ "$deploy_rc" -eq 0 ] &&
   [ -f "$deploy_target/public/css/app.css" ] &&
   [ -f "$deploy_target/src/index.php" ] &&
   [ -z "$forbidden" ]; then
  pass "php-deploy sync (exclusions honoured)"
else
  fail "php-deploy sync" "exit $deploy_rc, missing files or leaked vendor/.zed/*.log${forbidden:+ ($forbidden)}"
fi
rm -rf "$deploy_target"

# ---------------------------------------------------------------- PHPUnit
phpunit_out=$(cd "$ROOT/fixtures/demo" && ./vendor/bin/phpunit tests 2>&1)
phpunit_rc=$?
if [ "$phpunit_rc" -eq 0 ] && [[ "$phpunit_out" == *"OK ("* ]]; then
  pass "phpunit fixtures/demo"
else
  fail "phpunit fixtures/demo" "exit $phpunit_rc, expected OK"
fi

# ---------------------------------------------------------------- cargo test
cargo_out=$(cd "$ROOT/tools/php-db" && cargo test 2>&1)
cargo_rc=$?
if [ "$cargo_rc" -eq 0 ] && [[ "$cargo_out" == *"test result: ok"* ]]; then
  pass "cargo test tools/php-db"
else
  fail "cargo test tools/php-db" "exit $cargo_rc, expected 'test result: ok'"
fi

# ---------------------------------------------------------------- summary
printf '\nSummary: %d passed, %d failed, %d skipped\n' "$PASS" "$FAIL" "$SKIP"
if [ "$FAIL" -gt 0 ]; then
  for failure in "${FAILURES[@]}"; do
    printf '  - %s\n' "$failure"
  done
  exit 1
fi
exit 0
