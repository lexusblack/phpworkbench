#!/usr/bin/env bash
#
# Build native Linux ELF binaries for PhpWorkbench using the phpworkbench-packager Docker container.
# Outputs to dist/linux/bin:
#   - php-db (Linux ELF 64-bit)
#   - php-deploy (Linux ELF 64-bit)
#   - php-http (Linux ELF 64-bit)
#   - zed / phpworkbench (Linux ELF 64-bit GUI binary)
#

set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DIST_LINUX_BIN="$ROOT/dist/linux/bin"
mkdir -p "$DIST_LINUX_BIN"

VENDOR_DIR="$ROOT/vendor"
if [ -L "$VENDOR_DIR" ]; then
    REAL_VENDOR=$(readlink "$VENDOR_DIR")
else
    REAL_VENDOR="$VENDOR_DIR"
fi

echo "=============================================="
echo "  Building Linux ELF Binaries in Docker"
echo "=============================================="

# 1. Build CLI helper tools
echo ">>> Building Linux ELF helper tools (php-db, php-deploy, php-http)..."
docker run --rm \
    -v "$ROOT":/workspace \
    -v "$HOME/.cargo/registry":/root/.cargo/registry \
    -v "$HOME/.cargo/git":/root/.cargo/git \
    phpworkbench-packager:latest bash -c "
set -e
mkdir -p /workspace/dist/linux/bin /tmp/target-tools

for tool in php-db php-deploy php-http; do
    echo \"  Compiling \$tool...\"
    cargo build --release --manifest-path /workspace/tools/\$tool/Cargo.toml --target-dir /tmp/target-tools
    cp /tmp/target-tools/release/\$tool /workspace/dist/linux/bin/
    strip /workspace/dist/linux/bin/\$tool
done
"

echo ">>> Helper tools built successfully:"
ls -lh "$DIST_LINUX_BIN"

# 2. Build Zed GUI binary for Linux if requested or needed
if [ "${BUILD_ZED_LINUX:-0}" = "1" ] || [ ! -f "$DIST_LINUX_BIN/zed" ]; then
    echo ">>> Building Linux ELF Zed GUI IDE binary..."
    docker run --rm \
        -v "$ROOT":/workspace \
        -v "$REAL_VENDOR":/workspace/vendor \
        -v "$HOME/.cargo/registry":/root/.cargo/registry \
        -v "$HOME/.cargo/git":/root/.cargo/git \
        phpworkbench-packager:latest bash -c "
set -e
cd /workspace/vendor/zed
echo \"  Compiling zed GUI in release mode...\"
cargo build --release --manifest-path Cargo.toml --package zed --bin zed --target-dir /tmp/target-zed
cp /tmp/target-zed/release/zed /workspace/dist/linux/bin/zed
strip /workspace/dist/linux/bin/zed
"
fi

echo "=============================================="
echo "  Linux ELF Binaries Ready in $DIST_LINUX_BIN:"
echo "=============================================="
file "$DIST_LINUX_BIN"/*
