#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DIST_DIR="$ROOT/dist"
APP_NAME="PhpWorkbench"
VERSION="${PHPWORKBENCH_VERSION:-$(cat "$ROOT/VERSION" 2>/dev/null | tr -d '[:space:]' || echo 1.0.1)}"
APP_BUNDLE="$DIST_DIR/$APP_NAME.app"
DMG_NAME="${APP_NAME}-${VERSION}.dmg"
DMG_PATH="$DIST_DIR/$DMG_NAME"
STAGE_DIR=$(mktemp -d "/tmp/${APP_NAME}_dmg.XXXXXX")

trap 'rm -rf "$STAGE_DIR"' EXIT

echo "=== Packaging $APP_NAME DMG ==="
if [ ! -d "$APP_BUNDLE" ]; then
    echo "Error: $APP_BUNDLE does not exist. Run bundle-app.sh first." >&2
    exit 1
fi

echo "1. Staging DMG contents in $STAGE_DIR..."
cp -R "$APP_BUNDLE" "$STAGE_DIR/"
ln -s /Applications "$STAGE_DIR/Applications"

echo "2. Creating compressed DMG image at $DMG_PATH..."
rm -f "$DMG_PATH"
hdiutil create \
    -volname "$APP_NAME" \
    -srcfolder "$STAGE_DIR" \
    -ov \
    -format UDZO \
    "$DMG_PATH"

echo "3. Code signing DMG..."
codesign --force --sign - "$DMG_PATH" || true

ln -sf "$DMG_NAME" "$DIST_DIR/$APP_NAME.dmg"

echo "=== Successfully built DMG: $DMG_PATH ==="
ls -lh "$DMG_PATH"
