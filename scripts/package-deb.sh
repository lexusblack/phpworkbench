#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DIST_DIR="$ROOT/dist"
PKG_NAME="phpworkbench"
VERSION="${PHPWORKBENCH_VERSION:-$(cat "$ROOT/VERSION" 2>/dev/null | tr -d '[:space:]' || echo 1.0.3)}"
ARCH="${1:-arm64}"
DEB_NAME="${PKG_NAME}_${VERSION}_${ARCH}.deb"
DEB_PATH="$DIST_DIR/$DEB_NAME"
STAGE_DIR=$(mktemp -d "/tmp/${PKG_NAME}_deb.XXXXXX")

trap 'rm -rf "$STAGE_DIR"' EXIT

echo "=== Packaging $PKG_NAME $VERSION ($ARCH) DEB ==="
mkdir -p "$DIST_DIR"

# 1. Create directory tree
echo "1. Staging package hierarchy..."
mkdir -p "$STAGE_DIR/DEBIAN"
mkdir -p "$STAGE_DIR/usr/bin"
mkdir -p "$STAGE_DIR/usr/lib/$PKG_NAME"
mkdir -p "$STAGE_DIR/usr/share/applications"
mkdir -p "$STAGE_DIR/usr/share/pixmaps"
mkdir -p "$STAGE_DIR/usr/share/doc/$PKG_NAME"

for size in 64 128 256 512 1024; do
    mkdir -p "$STAGE_DIR/usr/share/icons/hicolor/${size}x${size}/apps"
done

# 2. Control file
echo "2. Writing DEBIAN/control..."
cat <<EOF > "$STAGE_DIR/DEBIAN/control"
Package: $PKG_NAME
Version: $VERSION
Section: devel
Priority: optional
Architecture: $ARCH
Maintainer: PhpWorkbench Team <support@phpworkbench.dev>
Installed-Size: 1048576
Depends: libc6 (>= 2.31), libssl3 | libssl1.1, libvulkan1, libxkbcommon0
Homepage: https://phpworkbench.dev
Description: PhpWorkbench - High Performance PHP IDE
 PhpWorkbench is a modern PHP development environment pairing JetBrains
 ergonomics (keymaps, 12 authentic menus, tool window navigation) with
 GPU-accelerated rendering.
 Includes integrated Database client, Remote SFTP/FTP server deployment,
 HTTP REST Client, and full PHP toolchain support.
EOF

# 3. Post-install and pre-remove scripts
echo "3. Writing maintainer scripts..."
cat <<'EOF' > "$STAGE_DIR/DEBIAN/postinst"
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q /usr/share/icons/hicolor || true
fi
if command -v update-mime-database >/dev/null 2>&1; then
    update-mime-database /usr/share/mime || true
fi
exit 0
EOF
chmod 755 "$STAGE_DIR/DEBIAN/postinst"

cat <<'EOF' > "$STAGE_DIR/DEBIAN/prerm"
#!/bin/sh
set -e
exit 0
EOF
chmod 755 "$STAGE_DIR/DEBIAN/prerm"

# 4. Binaries & Launchers
echo "4. Installing binaries & launchers..."
cat <<'EOF' > "$STAGE_DIR/usr/bin/phpworkbench"
#!/usr/bin/env bash
# PhpWorkbench IDE Launcher
APP_DIR="/usr/lib/phpworkbench"
export PATH="$APP_DIR/bin:$PATH"

if [ -x "$APP_DIR/bin/phpworkbench" ]; then
    exec "$APP_DIR/bin/phpworkbench" "$@"
elif [ -x "$APP_DIR/bin/zed" ]; then
    exec "$APP_DIR/bin/zed" "$@"
elif [ -x "$APP_DIR/libexec/zed-editor" ]; then
    exec "$APP_DIR/libexec/zed-editor" "$@"
else
    echo "PhpWorkbench launcher: executable not found in $APP_DIR/bin" >&2
    exit 1
fi
EOF
chmod 755 "$STAGE_DIR/usr/bin/phpworkbench"

mkdir -p "$STAGE_DIR/usr/lib/$PKG_NAME/bin"

# 1. Main IDE binary (custom compiled Linux ELF)
case "$ARCH" in
    arm64|aarch64) ENGINE_ARCH="aarch64" ;;
    amd64|x86_64)  ENGINE_ARCH="x86_64" ;;
    *)             ENGINE_ARCH="$ARCH" ;;
esac

ARCH_BIN_DIR="$ROOT/dist/linux/bin/$ENGINE_ARCH"
GENERIC_BIN_DIR="$ROOT/dist/linux/bin"

if [ -f "$ARCH_BIN_DIR/phpworkbench" ]; then
    cp "$ARCH_BIN_DIR/phpworkbench" "$STAGE_DIR/usr/lib/$PKG_NAME/bin/phpworkbench"
elif [ -f "$ARCH_BIN_DIR/zed" ]; then
    cp "$ARCH_BIN_DIR/zed" "$STAGE_DIR/usr/lib/$PKG_NAME/bin/phpworkbench"
elif [ -f "$GENERIC_BIN_DIR/phpworkbench" ]; then
    cp "$GENERIC_BIN_DIR/phpworkbench" "$STAGE_DIR/usr/lib/$PKG_NAME/bin/phpworkbench"
elif [ -f "$GENERIC_BIN_DIR/zed" ]; then
    cp "$GENERIC_BIN_DIR/zed" "$STAGE_DIR/usr/lib/$PKG_NAME/bin/phpworkbench"
elif [ "${ALLOW_MISSING_BINARIES:-0}" != "1" ]; then
    echo "Error: custom compiled Linux ELF IDE binary not found in $ARCH_BIN_DIR or $GENERIC_BIN_DIR" >&2
    exit 1
fi

if [ -f "$STAGE_DIR/usr/lib/$PKG_NAME/bin/phpworkbench" ]; then
    chmod 755 "$STAGE_DIR/usr/lib/$PKG_NAME/bin/phpworkbench"
    echo "  Installed custom Linux ELF IDE binary ($ENGINE_ARCH)"
fi

# 2. Helper tools (Linux ELF php-db, php-deploy, php-http)
for tool in php-db php-deploy php-http; do
    if [ -f "$ARCH_BIN_DIR/$tool" ]; then
        SRC_TOOL="$ARCH_BIN_DIR/$tool"
    elif [ -f "$GENERIC_BIN_DIR/$tool" ]; then
        SRC_TOOL="$GENERIC_BIN_DIR/$tool"
    elif [ "${ALLOW_MISSING_BINARIES:-0}" = "1" ]; then
        continue
    else
        echo "Error: Linux ELF helper binary '$tool' not found in $ARCH_BIN_DIR or $GENERIC_BIN_DIR" >&2
        exit 1
    fi
    cp "$SRC_TOOL" "$STAGE_DIR/usr/bin/$tool"
    cp "$SRC_TOOL" "$STAGE_DIR/usr/lib/$PKG_NAME/bin/$tool"
    chmod 755 "$STAGE_DIR/usr/bin/$tool" "$STAGE_DIR/usr/lib/$PKG_NAME/bin/$tool"
    echo "  Installed Linux ELF $tool ($ENGINE_ARCH)"
done

# Copy profile resources if present alongside compiled embedded assets
if [ -d "$ROOT/profile/themes" ]; then
    mkdir -p "$STAGE_DIR/usr/lib/$PKG_NAME/resources/themes"
    cp -R "$ROOT/profile/themes/"* "$STAGE_DIR/usr/lib/$PKG_NAME/resources/themes/"
fi

# 5. Desktop file
echo "5. Writing desktop entry..."
cat <<EOF > "$STAGE_DIR/usr/share/applications/phpworkbench.desktop"
[Desktop Entry]
Name=PhpWorkbench
Comment=PHP IDE with JetBrains ergonomics and GPUI speed
GenericName=PHP IDE
Exec=/usr/bin/phpworkbench %F
Icon=phpworkbench
Terminal=false
Type=Application
Categories=Development;IDE;TextEditor;
MimeType=text/plain;text/x-php;application/x-php;
StartupNotify=true
StartupWMClass=phpworkbench
Keywords=php;ide;editor;workbench;phpstorm;
EOF
chmod 644 "$STAGE_DIR/usr/share/applications/phpworkbench.desktop"

# 6. Icons
echo "6. Generating icons..."
ICON_SRC="$ROOT/design/branding/tempest_app_icon_1024.png"
if [ -f "$ICON_SRC" ]; then
    cp "$ICON_SRC" "$STAGE_DIR/usr/share/icons/hicolor/1024x1024/apps/phpworkbench.png"
    cp "$ICON_SRC" "$STAGE_DIR/usr/share/pixmaps/phpworkbench.png"
    for size in 64 128 256 512; do
        OUT_PNG="$STAGE_DIR/usr/share/icons/hicolor/${size}x${size}/apps/phpworkbench.png"
        if command -v sips >/dev/null 2>&1; then
            sips -z "$size" "$size" "$ICON_SRC" --out "$OUT_PNG" >/dev/null 2>&1 || true
        elif python3 -c "import PIL" 2>/dev/null; then
            python3 -c "from PIL import Image; Image.open('$ICON_SRC').resize(($size, $size), Image.Resampling.LANCZOS).save('$OUT_PNG')" >/dev/null 2>&1 || true
        fi
    done
fi

# 7. Documentation
echo "7. Writing documentation..."
cat <<EOF > "$STAGE_DIR/usr/share/doc/$PKG_NAME/copyright"
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: PhpWorkbench
Source: https://phpworkbench.dev

Files: *
Copyright: 2026 PhpWorkbench Contributors
License: Apache-2.0 or MIT
EOF

cat <<EOF > "$STAGE_DIR/usr/share/doc/$PKG_NAME/README.md"
# PhpWorkbench

PhpWorkbench is a high-performance PHP IDE pairing JetBrains ergonomics with GPUI rendering speed.

- Complete 12 PhpStorm menus and keybindings
- Integrated Database explorer and SQL console
- Remote server deployment via SFTP / FTP
- Built-in HTTP REST Client
EOF

# 8. Build Debian Package
echo "8. Building deb package..."
rm -f "$DEB_PATH"

if command -v dpkg-deb >/dev/null 2>&1; then
    echo "Using native dpkg-deb..."
    dpkg-deb --build --root-owner-group "$STAGE_DIR" "$DEB_PATH"
elif docker info >/dev/null 2>&1; then
    echo "Using Docker dpkg-deb for certified Debian package..."
    docker run --rm \
        -v "$STAGE_DIR":/stage \
        -v "$DIST_DIR":/out \
        ubuntu:22.04 \
        dpkg-deb --build --root-owner-group /stage "/out/$DEB_NAME"
else
    echo "Docker not available, using python pure-deb packaging..."
    python3 - <<PYEOF
import os, tarfile, time

stage = "$STAGE_DIR"
out_deb = "$DEB_PATH"

# Generate control.tar.gz
control_tgz = os.path.join(stage, "control.tar.gz")
with tarfile.open(control_tgz, "w:gz") as tar:
    for item in os.listdir(os.path.join(stage, "DEBIAN")):
        p = os.path.join(stage, "DEBIAN", item)
        tar.add(p, arcname=f"./{item}")

# Generate data.tar.gz
data_tgz = os.path.join(stage, "data.tar.gz")
with tarfile.open(data_tgz, "w:gz") as tar:
    for item in ["usr"]:
        p = os.path.join(stage, item)
        if os.path.exists(p):
            tar.add(p, arcname=f"./{item}")

# Create deb (ar archive)
with open(out_deb, "wb") as f:
    f.write(b"!<arch>\n")
    # debian-binary
    db = b"2.0\n"
    f.write(f"{'debian-binary':<16}{'0':<12}{'0':<6}{'0':<6}{'100644':<8}{len(db):<10}`\n".encode("ascii"))
    f.write(db)
    # control.tar.gz
    with open(control_tgz, "rb") as cf:
        cdata = cf.read()
    pad = b"\n" if len(cdata) % 2 != 0 else b""
    f.write(f"{'control.tar.gz':<16}{'0':<12}{'0':<6}{'0':<6}{'100644':<8}{len(cdata):<10}`\n".encode("ascii"))
    f.write(cdata + pad)
    # data.tar.gz
    with open(data_tgz, "rb") as df:
        ddata = df.read()
    pad = b"\n" if len(ddata) % 2 != 0 else b""
    f.write(f"{'data.tar.gz':<16}{'0':<12}{'0':<6}{'0':<6}{'100644':<8}{len(ddata):<10}`\n".encode("ascii"))
    f.write(ddata + pad)
PYEOF
fi

# Create generic symlink
ln -sf "$DEB_NAME" "$DIST_DIR/phpworkbench.deb"

echo "=== Successfully built DEB: $DEB_PATH ==="
ls -lh "$DEB_PATH"
