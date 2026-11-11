#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DIST_DIR="$ROOT/dist"
PKG_NAME="phpworkbench"
VERSION="1.0.0"
RELEASE="1"
ARCH="${1:-aarch64}"
RPM_NAME="${PKG_NAME}-${VERSION}-${RELEASE}.${ARCH}.rpm"
RPM_PATH="$DIST_DIR/$RPM_NAME"
STAGE_DIR=$(mktemp -d "/tmp/${PKG_NAME}_rpm.XXXXXX")

trap 'rm -rf "$STAGE_DIR"' EXIT

echo "=== Packaging $PKG_NAME $VERSION-$RELEASE ($ARCH) RPM ==="
mkdir -p "$DIST_DIR"

# 1. Setup rpmbuild directory structure
RPMROOT="$STAGE_DIR/rpmbuild"
mkdir -p "$RPMROOT"/{BUILD,RPMS,SOURCES,SPECS,SRPMS,BUILDROOT}
INSTALL_ROOT="$STAGE_DIR/staged_files"
mkdir -p "$INSTALL_ROOT/usr/bin"
mkdir -p "$INSTALL_ROOT/usr/lib/$PKG_NAME"
mkdir -p "$INSTALL_ROOT/usr/share/applications"
mkdir -p "$INSTALL_ROOT/usr/share/pixmaps"
mkdir -p "$INSTALL_ROOT/usr/share/doc/$PKG_NAME"

for size in 64 128 256 512 1024; do
    mkdir -p "$INSTALL_ROOT/usr/share/icons/hicolor/${size}x${size}/apps"
done

# 2. Launcher & CLI tools
echo "1. Staging binaries & launcher..."
cat <<'EOF' > "$INSTALL_ROOT/usr/bin/phpworkbench"
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
chmod 755 "$INSTALL_ROOT/usr/bin/phpworkbench"

mkdir -p "$INSTALL_ROOT/usr/lib/$PKG_NAME/bin"

# 1. Main IDE binary & engine (Linux ELF)
case "$ARCH" in
    arm64|aarch64) ENGINE_ARCH="aarch64" ;;
    amd64|x86_64)  ENGINE_ARCH="x86_64" ;;
    *)             ENGINE_ARCH="$ARCH" ;;
esac

ENGINE_DIR="$ROOT/dist/linux/engines/$ENGINE_ARCH/zed.app"

if [ -d "$ENGINE_DIR" ]; then
    echo "  Installing Linux ELF IDE engine from $ENGINE_DIR ($ENGINE_ARCH)..."
    mkdir -p "$INSTALL_ROOT/usr/lib/$PKG_NAME/libexec"
    mkdir -p "$INSTALL_ROOT/usr/lib/$PKG_NAME/lib"
    cp -R "$ENGINE_DIR/bin"/* "$INSTALL_ROOT/usr/lib/$PKG_NAME/bin/"
    cp -R "$ENGINE_DIR/libexec"/* "$INSTALL_ROOT/usr/lib/$PKG_NAME/libexec/"
    cp -R "$ENGINE_DIR/lib"/* "$INSTALL_ROOT/usr/lib/$PKG_NAME/lib/"
    chmod 755 "$INSTALL_ROOT/usr/lib/$PKG_NAME/bin"/* "$INSTALL_ROOT/usr/lib/$PKG_NAME/libexec"/*
    echo "  Installed complete Linux IDE engine (bin/zed, libexec/zed-editor, lib/)"
elif [ -f "$ROOT/dist/linux/bin/zed" ]; then
    cp "$ROOT/dist/linux/bin/zed" "$INSTALL_ROOT/usr/lib/$PKG_NAME/bin/zed"
    chmod 755 "$INSTALL_ROOT/usr/lib/$PKG_NAME/bin/zed"
    echo "  Installed main IDE binary: zed"
elif [ -f "$ROOT/dist/linux/bin/phpworkbench" ]; then
    cp "$ROOT/dist/linux/bin/phpworkbench" "$INSTALL_ROOT/usr/lib/$PKG_NAME/bin/phpworkbench"
    chmod 755 "$INSTALL_ROOT/usr/lib/$PKG_NAME/bin/phpworkbench"
    echo "  Installed main IDE binary: phpworkbench"
else
    echo "  Warning: No Linux ELF zed engine found" >&2
fi

# 2. Helper tools (Linux ELF php-db, php-deploy, php-http)
for tool in php-db php-deploy php-http; do
    if [ -f "$ROOT/dist/linux/bin/$tool" ]; then
        cp "$ROOT/dist/linux/bin/$tool" "$INSTALL_ROOT/usr/bin/$tool"
        cp "$ROOT/dist/linux/bin/$tool" "$INSTALL_ROOT/usr/lib/$PKG_NAME/bin/$tool"
        chmod 755 "$INSTALL_ROOT/usr/bin/$tool" "$INSTALL_ROOT/usr/lib/$PKG_NAME/bin/$tool"
        echo "  Installed Linux ELF $tool"
    fi
done

# Resources / assets
if [ -d "$ROOT/dist/PhpWorkbench.app/Contents/Resources" ]; then
    cp -R "$ROOT/dist/PhpWorkbench.app/Contents/Resources" "$INSTALL_ROOT/usr/lib/$PKG_NAME/resources"
fi

# 3. Desktop entry
echo "2. Writing desktop entry..."
cat <<EOF > "$INSTALL_ROOT/usr/share/applications/phpworkbench.desktop"
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
chmod 644 "$INSTALL_ROOT/usr/share/applications/phpworkbench.desktop"

# 4. Icons
echo "3. Generating icons..."
ICON_SRC="$ROOT/design/branding/tempest_app_icon_1024.png"
if [ -f "$ICON_SRC" ]; then
    cp "$ICON_SRC" "$INSTALL_ROOT/usr/share/icons/hicolor/1024x1024/apps/phpworkbench.png"
    cp "$ICON_SRC" "$INSTALL_ROOT/usr/share/pixmaps/phpworkbench.png"
    for size in 64 128 256 512; do
        sips -z "$size" "$size" "$ICON_SRC" --out "$INSTALL_ROOT/usr/share/icons/hicolor/${size}x${size}/apps/phpworkbench.png" >/dev/null 2>&1 || true
    done
fi

# 5. Documentation
echo "4. Writing documentation..."
cat <<EOF > "$INSTALL_ROOT/usr/share/doc/$PKG_NAME/copyright"
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: PhpWorkbench
Source: https://phpworkbench.dev

Files: *
Copyright: 2026 PhpWorkbench Contributors
License: Apache-2.0 or MIT
EOF

cat <<EOF > "$INSTALL_ROOT/usr/share/doc/$PKG_NAME/README.md"
# PhpWorkbench

PhpWorkbench is a high-performance PHP IDE pairing JetBrains ergonomics with GPUI rendering speed.

- Complete 12 PhpStorm menus and keybindings
- Integrated Database explorer and SQL console
- Remote server deployment via SFTP / FTP
- Built-in HTTP REST Client
EOF

# 6. RPM Spec file
echo "5. Writing RPM spec..."
cat <<SPEC > "$RPMROOT/SPECS/$PKG_NAME.spec"
Name:           $PKG_NAME
Version:        $VERSION
Release:        $RELEASE
Summary:        PhpWorkbench - High Performance PHP IDE
License:        Apache-2.0 or MIT
URL:            https://phpworkbench.dev
BuildArch:      $ARCH

%define debug_package %{nil}
%define _build_id_links none
%define __strip /bin/true
%define __brp_strip %{nil}
%define __brp_strip_static_archive %{nil}
%define __brp_strip_comment_note %{nil}

%description
PhpWorkbench is a modern PHP development environment pairing JetBrains
ergonomics (keymaps, 12 authentic menus, tool window navigation) with
GPU-accelerated rendering.
Includes integrated Database client, Remote SFTP/FTP server deployment,
HTTP REST Client, and full PHP toolchain support.

%install
rm -rf %{buildroot}
mkdir -p %{buildroot}
cp -R /staged_files/* %{buildroot}/

%post
if [ -x /usr/bin/update-desktop-database ]; then
    /usr/bin/update-desktop-database -q /usr/share/applications || true
fi
if [ -x /usr/bin/gtk-update-icon-cache ]; then
    /usr/bin/gtk-update-icon-cache -q /usr/share/icons/hicolor || true
fi

%postun
if [ -x /usr/bin/update-desktop-database ]; then
    /usr/bin/update-desktop-database -q /usr/share/applications || true
fi
if [ -x /usr/bin/gtk-update-icon-cache ]; then
    /usr/bin/gtk-update-icon-cache -q /usr/share/icons/hicolor || true
fi

%files
/usr/bin/phpworkbench
/usr/bin/php-db
/usr/bin/php-deploy
/usr/bin/php-http
/usr/lib/%{name}
/usr/share/applications/%{name}.desktop
/usr/share/icons/hicolor/*/apps/%{name}.png
/usr/share/pixmaps/%{name}.png
/usr/share/doc/%{name}

%changelog
* Tue Nov 10 2026 PhpWorkbench Team <support@phpworkbench.dev> - 1.0.0-1
- Initial release of PhpWorkbench with full PhpStorm keymaps and tool window system.
SPEC

# 7. Build RPM
echo "6. Running rpmbuild..."
rm -f "$RPM_PATH"

if docker info >/dev/null 2>&1; then
    docker run --rm \
        -v "$RPMROOT":/rpmroot \
        -v "$INSTALL_ROOT":/staged_files \
        -v "$DIST_DIR":/out \
        phpworkbench-packager \
        bash -c "
            rpmbuild --define '_topdir /rpmroot' --target "$ARCH" -bb /rpmroot/SPECS/$PKG_NAME.spec
            cp /rpmroot/RPMS/$ARCH/*.rpm /out/$RPM_NAME
        "
else
    echo "Error: Docker required for rpmbuild on macOS" >&2
    exit 1
fi

ln -sf "$RPM_NAME" "$DIST_DIR/phpworkbench.rpm"

echo "=== Successfully built RPM: $RPM_PATH ==="
ls -lh "$RPM_PATH"
