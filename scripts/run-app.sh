#!/usr/bin/env bash
#
# Launch the packaged PhpWorkbench.app with an isolated data directory.
#
# Usage: scripts/run-app.sh [args...]
#
# Data dir: $ZED_USER_DATA_DIR when set, otherwise ~/.phpworkbench.
# All arguments are forwarded to the app executable.

set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
APP="$ROOT/dist/PhpWorkbench.app/Contents/MacOS/PhpWorkbench"
DATA="${ZED_USER_DATA_DIR:-$HOME/.phpworkbench}"

if [ ! -x "$APP" ]; then
  printf 'error: app executable not found at %s\n' "$APP" >&2
  printf 'run scripts/bundle-app.sh first\n' >&2
  exit 1
fi

mkdir -p "$DATA"
exec "$APP" --user-data-dir "$DATA" "$@"
