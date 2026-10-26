#!/usr/bin/env bash
set -euo pipefail

ZED_TAG="${ZED_TAG:-v1.25.3}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENDOR="$ROOT/vendor"
ZED="$VENDOR/zed"

export PATH="/opt/homebrew/bin:$PATH"

command -v cmake >/dev/null || { echo "cmake required: brew install cmake"; exit 1; }
xcrun metal --version >/dev/null 2>&1 || { echo "Metal toolchain required: xcodebuild -downloadComponent MetalToolchain"; exit 1; }

mkdir -p "$VENDOR"

if [ ! -d "$ZED/.git" ]; then
  echo "clone zed $ZED_TAG"
  git clone --depth 1 --branch "$ZED_TAG" https://github.com/zed-industries/zed.git "$ZED"
else
  echo "update zed $ZED_TAG"
  git -C "$ZED" fetch --tags --depth 1 origin "refs/tags/$ZED_TAG:refs/tags/$ZED_TAG" || true
  git -C "$ZED" checkout "$ZED_TAG"
fi

echo "toolchain:"
cat "$ZED/rust-toolchain.toml" 2>/dev/null || true

echo "targets"
rustup target add wasm32-wasip2 wasm32-unknown-unknown

echo "build"
cd "$ZED"
cargo build -p zed

echo "ready: cd vendor/zed && cargo run -p zed"
