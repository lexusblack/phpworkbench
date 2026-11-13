#!/usr/bin/env bash
#
# Build a release Zed binary and package a minimal macOS .app.
#
# Usage: scripts/bundle-app.sh [--dry-run]
#
# Prefers Zed's own vendor/zed/script/bundle-mac when present. The fallback
# copies the release binary into dist/PhpWorkbench.app and writes a minimal
# Info.plist; those debug/extra assets are NOT embedded, so use the native
# bundle script for anything you intend to ship.
#
# The release build is intentionally NOT run by default validation; pass
# --dry-run to see the exact commands.

set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
ZED_ROOT="$ROOT/vendor/zed"
APP_NAME="${APP_NAME:-PhpWorkbench}"
DIST_DIR="$ROOT/dist"
DRY_RUN=0

for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    -h|--help)
      sed -n '2,13p' "$0"
      exit 0
      ;;
    *)
      printf 'unknown argument: %s\n' "$arg" >&2
      exit 2
      ;;
  esac
done

say() {
  printf '%s\n' "$*"
}

run_in() {
  local dir=$1
  shift
  if [ "$DRY_RUN" -eq 1 ]; then
    printf 'DRY-RUN: (cd %q &&' "$dir"
    printf ' %q' "$@"
    printf ')\n'
  else
    (cd "$dir" && "$@")
  fi
}

if [ ! -f "$ZED_ROOT/Cargo.toml" ]; then
  printf 'error: Zed checkout not found at %s\n' "$ZED_ROOT" >&2
  printf 'run scripts/bootstrap-zed.sh first\n' >&2
  exit 1
fi

say "Project root: $ROOT"
say "Zed checkout: $ZED_ROOT"
say "App name:     $APP_NAME"

# 1. Release build (cargo build --release -p zed).
say "Step 1: cargo build --release -p zed"
run_in "$ZED_ROOT" cargo build --release -p zed

# 2. Package with Zed's native macOS bundle script when available.
BUNDLE_SCRIPT="$ZED_ROOT/script/bundle-mac"
if [ -x "$BUNDLE_SCRIPT" ]; then
  say "Step 2: packaging with Zed's script/bundle-mac"
  run_in "$ZED_ROOT" ./script/bundle-mac
  if [ "$DRY_RUN" -eq 1 ]; then
    say "DRY-RUN: would find the produced .app under $ZED_ROOT/target"
  else
    APP_PATH=$(find "$ZED_ROOT/target" -maxdepth 3 -type d -name '*.app' \
      -print 2>/dev/null | head -n 1 || true)
    if [ -n "$APP_PATH" ]; then
      say "Bundle created: $APP_PATH"
    else
      printf 'warning: bundle-mac finished but no .app was found under %s/target\n' \
        "$ZED_ROOT" >&2
    fi
  fi
  exit 0
fi

# 3. Fallback: minimal .app around target/release/zed.
say "Step 2: script/bundle-mac not executable; using minimal fallback"
BIN="$ZED_ROOT/target/release/zed"
APP_DIR="$DIST_DIR/$APP_NAME.app"

if [ "$DRY_RUN" -eq 1 ]; then
  say "DRY-RUN: would verify $BIN exists"
  say "DRY-RUN: would create $APP_DIR/Contents/{MacOS,Resources}"
  say "DRY-RUN: would copy $BIN -> $APP_DIR/Contents/MacOS/$APP_NAME"
  say "DRY-RUN: would write $APP_DIR/Contents/Info.plist"
  say "DRY-RUN: warning: debug assets are not embedded in this minimal bundle"
  exit 0
fi

if [ ! -f "$BIN" ]; then
  printf 'error: release binary not found at %s\n' "$BIN" >&2
  printf 'the release build above may have failed\n' >&2
  exit 1
fi

mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"
cp "$BIN" "$APP_DIR/Contents/MacOS/$APP_NAME"

cat >"$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key>
  <string>$APP_NAME</string>
  <key>CFBundleIdentifier</key>
  <string>dev.phpworkbench.$APP_NAME</string>
  <key>CFBundleName</key>
  <string>$APP_NAME</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>${PHPWORKBENCH_VERSION:-1.0.2}</string>
  <key>LSMinimumSystemVersion</key>
  <string>10.15.7</string>
</dict>
</plist>
PLIST

printf 'warning: minimal bundle created at %s\n' "$APP_DIR" >&2
printf 'warning: debug assets are not embedded; use script/bundle-mac for a full bundle\n' >&2
say "Bundle created: $APP_DIR"
