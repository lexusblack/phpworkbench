#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DIST_DIR="$ROOT/dist"
PKG_NAME="phpworkbench"
VERSION="${PHPWORKBENCH_VERSION:-$(cat "$ROOT/VERSION" 2>/dev/null | tr -d '[:space:]' || echo 1.0.2)}"
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

# 1. Main IDE binary (custom compiled Linux ELF)
case "$ARCH" in
    arm64|aarch64) ENGINE_ARCH="aarch64" ;;
    amd64|x86_64)  ENGINE_ARCH="x86_64" ;;
    *)             ENGINE_ARCH="$ARCH" ;;
esac

ARCH_BIN_DIR="$ROOT/dist/linux/bin/$ENGINE_ARCH"
GENERIC_BIN_DIR="$ROOT/dist/linux/bin"

if [ -f "$ARCH_BIN_DIR/phpworkbench" ]; then
    cp "$ARCH_BIN_DIR/phpworkbench" "$INSTALL_ROOT/usr/lib/$PKG_NAME/bin/phpworkbench"
elif [ -f "$ARCH_BIN_DIR/zed" ]; then
    cp "$ARCH_BIN_DIR/zed" "$INSTALL_ROOT/usr/lib/$PKG_NAME/bin/phpworkbench"
elif [ -f "$GENERIC_BIN_DIR/phpworkbench" ]; then
    cp "$GENERIC_BIN_DIR/phpworkbench" "$INSTALL_ROOT/usr/lib/$PKG_NAME/bin/phpworkbench"
elif [ -f "$GENERIC_BIN_DIR/zed" ]; then
    cp "$GENERIC_BIN_DIR/zed" "$INSTALL_ROOT/usr/lib/$PKG_NAME/bin/phpworkbench"
elif [ "${ALLOW_MISSING_BINARIES:-0}" != "1" ]; then
    echo "Error: custom compiled Linux ELF IDE binary not found in $ARCH_BIN_DIR or $GENERIC_BIN_DIR" >&2
    exit 1
fi

if [ -f "$INSTALL_ROOT/usr/lib/$PKG_NAME/bin/phpworkbench" ]; then
    chmod 755 "$INSTALL_ROOT/usr/lib/$PKG_NAME/bin/phpworkbench"
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
    cp "$SRC_TOOL" "$INSTALL_ROOT/usr/bin/$tool"
    cp "$SRC_TOOL" "$INSTALL_ROOT/usr/lib/$PKG_NAME/bin/$tool"
    chmod 755 "$INSTALL_ROOT/usr/bin/$tool" "$INSTALL_ROOT/usr/lib/$PKG_NAME/bin/$tool"
    echo "  Installed Linux ELF $tool ($ENGINE_ARCH)"
done

# Profile resources
if [ -d "$ROOT/profile/themes" ]; then
    mkdir -p "$INSTALL_ROOT/usr/lib/$PKG_NAME/resources/themes"
    cp -R "$ROOT/profile/themes/"* "$INSTALL_ROOT/usr/lib/$PKG_NAME/resources/themes/"
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
        OUT_PNG="$INSTALL_ROOT/usr/share/icons/hicolor/${size}x${size}/apps/phpworkbench.png"
        if command -v sips >/dev/null 2>&1; then
            sips -z "$size" "$size" "$ICON_SRC" --out "$OUT_PNG" >/dev/null 2>&1 || true
        elif python3 -c "import PIL" 2>/dev/null; then
            python3 -c "from PIL import Image; Image.open('$ICON_SRC').resize(($size, $size), Image.Resampling.LANCZOS).save('$OUT_PNG')" >/dev/null 2>&1 || true
        fi
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
cp -R %{_staged_files}/* %{buildroot}/

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
* Fri Nov 13 2026 PhpWorkbench Team <support@phpworkbench.dev> - 1.0.2-1
- Add PhpStorm-style Git Branches popup with quick actions, hierarchical search, and branch submenus.

* Wed Nov 11 2026 PhpWorkbench Team <support@phpworkbench.dev> - 1.0.1-1
- Align default dock layout across platforms, add platform keybinding tooltips, and bump version to 1.0.1.

* Tue Nov 10 2026 PhpWorkbench Team <support@phpworkbench.dev> - 1.0.0-1
- Initial release of PhpWorkbench with full PhpStorm keymaps and tool window system.
SPEC

# 7. Build RPM
echo "6. Running rpmbuild..."
rm -f "$RPM_PATH"

if command -v rpmbuild >/dev/null 2>&1; then
    echo "Using native rpmbuild..."
    rpmbuild --define "_topdir $RPMROOT" --define "_staged_files $INSTALL_ROOT" --target "$ARCH" -bb "$RPMROOT/SPECS/$PKG_NAME.spec"
    cp "$RPMROOT/RPMS/$ARCH/"*.rpm "$RPM_PATH"
elif docker info >/dev/null 2>&1; then
    docker run --rm \
        -v "$RPMROOT":/rpmroot \
        -v "$INSTALL_ROOT":/staged_files \
        -v "$DIST_DIR":/out \
        phpworkbench-packager \
        bash -c "
            rpmbuild --define '_topdir /rpmroot' --define '_staged_files /staged_files' --target "$ARCH" -bb /rpmroot/SPECS/$PKG_NAME.spec
            cp /rpmroot/RPMS/$ARCH/*.rpm /out/$RPM_NAME
        "
else
    echo "Error: Docker required for rpmbuild on macOS" >&2
    exit 1
fi

ln -sf "$RPM_NAME" "$DIST_DIR/phpworkbench.rpm"

echo "=== Successfully built RPM: $RPM_PATH ==="
ls -lh "$RPM_PATH"
