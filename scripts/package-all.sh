#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
SCRIPTS_DIR="$ROOT/scripts"

echo "=============================================="
echo "  PhpWorkbench Multi-Platform Distribution"
echo "=============================================="

echo ">>> Pre-build: Building Linux ELF binaries in Docker..."
"$SCRIPTS_DIR/build-linux-binaries.sh"

echo ">>> 1/4: Packaging macOS DMG..."
"$SCRIPTS_DIR/package-dmg.sh"

echo ">>> 2/4: Packaging Debian/Ubuntu DEB..."
"$SCRIPTS_DIR/package-deb.sh" arm64
"$SCRIPTS_DIR/package-deb.sh" amd64

echo ">>> 3/4: Packaging Red Hat/Fedora RPM..."
"$SCRIPTS_DIR/package-rpm.sh" aarch64

echo ">>> 4/4: Packaging Windows Installer EXE..."
"$SCRIPTS_DIR/package-exe.sh"

echo "=============================================="
echo "  All distribution packages built in dist/:"
echo "=============================================="
ls -lh "$ROOT/dist"
