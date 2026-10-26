#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA_DIR="${1:?usage: install-deps-guard.sh <user-data-dir>}"
CLI="$ROOT/vendor/zed/target/debug/zed-extension"
SRC="$ROOT/extensions/deps-guard"
OUT="$ROOT/.cache/deps-guard-out"
SCRATCH="$ROOT/.cache/wasi-sdk"
LOG="$ROOT/.cache/deps-guard-build.log"

export PATH="/opt/homebrew/bin:$PATH"

command -v deps-audit >/dev/null || { echo "install deps-audit first: cargo install --path tools/deps-audit"; exit 1; }
[ -x "$CLI" ] || { echo "build extension_cli first: cargo build -p extension_cli (in vendor/zed)"; exit 1; }

mkdir -p "$OUT" "$SCRATCH"
rm -f "$SRC/extension.wasm"

"$CLI" --source-dir "$SRC" --output-dir "$OUT" --scratch-dir "$SCRATCH" > "$LOG" 2>&1 &
PID=$!

for _ in $(seq 1 150); do
  [ -f "$SRC/extension.wasm" ] && break
  sleep 2
done

kill "$PID" 2>/dev/null || true
wait "$PID" 2>/dev/null || true

[ -f "$SRC/extension.wasm" ] || { echo "build failed, see $LOG"; exit 1; }

DEST="$DATA_DIR/extensions/installed/deps-guard"
mkdir -p "$DEST"
cp "$SRC/extension.wasm" "$DEST/extension.wasm"
cp "$SRC/installed.toml" "$DEST/extension.toml"

echo "installed deps-guard into $DEST"
