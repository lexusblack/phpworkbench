#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ZED="$ROOT/vendor/zed"
BACKUPS="$ROOT/backups"

[ -d "$ZED/.git" ] || { echo "no fork at $ZED"; exit 1; }

mkdir -p "$BACKUPS" "$ROOT/recovery"
STAMP="$(date +%Y%m%d-%H%M%S)"
BUNDLE="$BACKUPS/zed-fork-$STAMP.bundle"

git -C "$ZED" bundle create "$BUNDLE" --all
cp "$ZED/crates/php_panel/src/php_panel.rs" "$ROOT/recovery/php_panel.rs"
cp "$ZED/crates/php_panel/Cargo.toml" "$ROOT/recovery/php_panel.Cargo.toml"

echo "bundle: $BUNDLE"
du -h "$BUNDLE"
