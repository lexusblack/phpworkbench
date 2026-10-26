#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA_DIR="${1:?usage: install-php-extension.sh <user-data-dir>}"
CACHE="$ROOT/.cache"
SRC="$CACHE/php-ext"
OUT="$CACHE/php-ext-out"
SCRATCH="$CACHE/wasi-sdk"
CLI="$ROOT/vendor/zed/target/debug/zed-extension"

export PATH="/opt/homebrew/bin:$PATH"

[ -x "$CLI" ] || { echo "build it first: cargo build -p extension_cli (in vendor/zed)"; exit 1; }
command -v cmake >/dev/null || { echo "cmake required"; exit 1; }

mkdir -p "$CACHE" "$OUT" "$SCRATCH"
rm -rf "$SRC"
git clone --depth 1 https://github.com/zed-extensions/php "$SRC"

"$CLI" --source-dir "$SRC" --output-dir "$OUT" --scratch-dir "$SCRATCH" > "$CACHE/php-ext-build.log" 2>&1 &
PID=$!

for _ in $(seq 1 180); do
  if [ -f "$SRC/extension.wasm" ] && [ -f "$SRC/grammars/php.wasm" ] && [ -f "$SRC/grammars/phpdoc.wasm" ]; then
    break
  fi
  sleep 2
done

kill "$PID" 2>/dev/null || true
wait "$PID" 2>/dev/null || true

[ -f "$SRC/extension.wasm" ] || { echo "extension.wasm was not produced, see $CACHE/php-ext-build.log"; exit 1; }

DEST="$DATA_DIR/extensions/installed/php"
mkdir -p "$DEST/grammars"
cp "$ROOT/extensions/php-installed/extension.toml" "$DEST/extension.toml"
cp "$SRC/extension.wasm" "$DEST/extension.wasm"
cp "$SRC/grammars/php.wasm" "$SRC/grammars/phpdoc.wasm" "$DEST/grammars/"
cp -R "$SRC/languages" "$DEST/languages"
cp -R "$SRC/debug_adapter_schemas" "$DEST/debug_adapter_schemas"

echo "installed PHP extension into $DEST"
