#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DIST_DIR="$ROOT/dist"
PKG_NAME="PhpWorkbench"
VERSION="${PHPWORKBENCH_VERSION:-$(cat "$ROOT/VERSION" 2>/dev/null | tr -d '[:space:]' || echo 1.0.3)}"
EXE_NAME="${PKG_NAME}-Setup-${VERSION}.exe"
EXE_PATH="$DIST_DIR/$EXE_NAME"
STAGE_DIR=$(mktemp -d "/tmp/${PKG_NAME}_exe.XXXXXX")

trap 'rm -rf "$STAGE_DIR"' EXIT

echo "=== Packaging $PKG_NAME $VERSION Windows Installer (EXE) ==="
mkdir -p "$DIST_DIR"

FILES_DIR="$STAGE_DIR/files"
mkdir -p "$FILES_DIR/bin"
mkdir -p "$FILES_DIR/resources"

# 1. Generate ICO from master PNG
echo "1. Generating Windows ICO icon..."
ICON_SRC="$ROOT/design/branding/tempest_app_icon_1024.png"
ICO_PATH="$STAGE_DIR/phpworkbench.ico"
python3 - <<PYEOF
from PIL import Image
im = Image.open("$ICON_SRC")
im.save("$ICO_PATH", format="ICO", sizes=[(256, 256), (128, 128), (64, 64), (48, 48), (32, 32), (16, 16)])
PYEOF

cp "$ICO_PATH" "$FILES_DIR/resources/phpworkbench.ico"

# 2. Binaries, CLI shim & files
echo "2. Staging Windows PE binaries & resources..."
BIN_SRC_DIR="$ROOT/dist/windows/bin"

if [ -f "$BIN_SRC_DIR/PhpWorkbench.exe" ]; then
    cp "$BIN_SRC_DIR/PhpWorkbench.exe" "$FILES_DIR/bin/PhpWorkbench.exe"
elif [ -f "$BIN_SRC_DIR/zed.exe" ]; then
    cp "$BIN_SRC_DIR/zed.exe" "$FILES_DIR/bin/PhpWorkbench.exe"
elif [ "${ALLOW_MISSING_BINARIES:-0}" != "1" ]; then
    echo "Error: compiled custom engine binary not found at $BIN_SRC_DIR/PhpWorkbench.exe or $BIN_SRC_DIR/zed.exe" >&2
    exit 1
fi

for extra in conpty.dll OpenConsole.exe; do
    if [ -f "$BIN_SRC_DIR/$extra" ]; then
        cp "$BIN_SRC_DIR/$extra" "$FILES_DIR/bin/$extra"
    fi
done

for tool in php-db php-deploy php-http; do
    if [ -f "$BIN_SRC_DIR/$tool.exe" ]; then
        cp "$BIN_SRC_DIR/$tool.exe" "$FILES_DIR/bin/$tool.exe"
    elif [ "${ALLOW_MISSING_BINARIES:-0}" != "1" ]; then
        echo "Error: compiled Windows helper binary not found at $BIN_SRC_DIR/$tool.exe" >&2
        exit 1
    fi
done

cat <<'EOF' > "$FILES_DIR/bin/phpworkbench.cmd"
@echo off
setlocal
start "" "%~dp0PhpWorkbench.exe" %*
endlocal
EOF

# Copy profile resources if present alongside compiled embedded assets
if [ -d "$ROOT/profile/themes" ]; then
    mkdir -p "$FILES_DIR/resources/themes"
    cp -R "$ROOT/profile/themes/"* "$FILES_DIR/resources/themes/"
fi

# License & Readme
cat <<EOF > "$STAGE_DIR/LICENSE.txt"
Apache License
Version 2.0, January 2004
http://www.apache.org/licenses/

TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION
Copyright 2026 PhpWorkbench Contributors.
EOF

cat <<EOF > "$FILES_DIR/README.txt"
PhpWorkbench - High Performance PHP IDE
=======================================
JetBrains ergonomics, 12 authentic menus, SQL console & database viewer,
remote server deployment (SFTP/FTP), and built-in HTTP REST client.

Website: https://phpworkbench.dev
EOF

# 3. Write NSIS script
echo "3. Writing NSIS installer configuration..."
cat <<NSI > "$STAGE_DIR/installer.nsi"
Unicode True
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!include "FileFunc.nsh"

!ifndef STAGE_DIR
  !define STAGE_DIR "/stage"
!endif
!ifndef OUT_FILE
  !define OUT_FILE "/out/$EXE_NAME"
!endif

Name "$PKG_NAME"
OutFile "\${OUT_FILE}"
InstallDir "\$LOCALAPPDATA\\Programs\\$PKG_NAME"
InstallDirRegKey HKCU "Software\\$PKG_NAME" "InstallDir"
RequestExecutionLevel user

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "$PKG_NAME"
VIAddVersionKey "CompanyName" "PhpWorkbench Team"
VIAddVersionKey "LegalCopyright" "2026 PhpWorkbench"
VIAddVersionKey "FileDescription" "PhpWorkbench Installer"
VIAddVersionKey "FileVersion" "${VERSION}.0"

!define MUI_ABORTWARNING
!define MUI_ICON "\${STAGE_DIR}/phpworkbench.ico"
!define MUI_UNICON "\${STAGE_DIR}/phpworkbench.ico"

; Installer Pages
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "\${STAGE_DIR}/LICENSE.txt"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

; Uninstaller Pages
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Section "PhpWorkbench (required)" SecCore
    SectionIn RO
    SetOutPath "\$INSTDIR"
    File /r "\${STAGE_DIR}/files/*"

    ; Create uninstaller
    WriteUninstaller "\$INSTDIR\\Uninstall.exe"

    ; Start Menu Shortcuts (launch PhpWorkbench.exe directly without cmd.exe console window)
    CreateDirectory "\$SMPROGRAMS\\$PKG_NAME"
    CreateShortcut "\$SMPROGRAMS\\$PKG_NAME\\$PKG_NAME.lnk" "\$INSTDIR\\bin\\PhpWorkbench.exe" "" "\$INSTDIR\\resources\\phpworkbench.ico" 0
    CreateShortcut "\$SMPROGRAMS\\$PKG_NAME\\Uninstall $PKG_NAME.lnk" "\$INSTDIR\\Uninstall.exe"

    ; Desktop Shortcut
    CreateShortcut "\$DESKTOP\\$PKG_NAME.lnk" "\$INSTDIR\\bin\\PhpWorkbench.exe" "" "\$INSTDIR\\resources\\phpworkbench.ico" 0

    ; Windows Add/Remove Programs Registry
    WriteRegStr HKCU "Software\\$PKG_NAME" "InstallDir" "\$INSTDIR"
    WriteRegStr HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\$PKG_NAME" "DisplayName" "$PKG_NAME"
    WriteRegStr HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\$PKG_NAME" "DisplayIcon" "\$INSTDIR\\resources\\phpworkbench.ico"
    WriteRegStr HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\$PKG_NAME" "DisplayVersion" "$VERSION"
    WriteRegStr HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\$PKG_NAME" "Publisher" "PhpWorkbench Team"
    WriteRegStr HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\$PKG_NAME" "UninstallString" '"\$INSTDIR\\Uninstall.exe"'
    WriteRegStr HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\$PKG_NAME" "QuietUninstallString" '"\$INSTDIR\\Uninstall.exe" /S'
SectionEnd

Section "Uninstall"
    RMDir /r "\$INSTDIR"
    Delete "\$SMPROGRAMS\\$PKG_NAME\\$PKG_NAME.lnk"
    Delete "\$SMPROGRAMS\\$PKG_NAME\\Uninstall $PKG_NAME.lnk"
    RMDir "\$SMPROGRAMS\\$PKG_NAME"
    Delete "\$DESKTOP\\$PKG_NAME.lnk"

    DeleteRegKey HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\$PKG_NAME"
    DeleteRegKey HKCU "Software\\$PKG_NAME"
SectionEnd
NSI

# 4. Build Windows Installer using makensis
echo "4. Running makensis..."
rm -f "$EXE_PATH"

if command -v makensis >/dev/null 2>&1; then
    makensis -DSTAGE_DIR="$STAGE_DIR" -DOUT_FILE="$EXE_PATH" "$STAGE_DIR/installer.nsi"
elif docker info >/dev/null 2>&1; then
    docker run --rm \
        -v "$STAGE_DIR":/stage \
        -v "$DIST_DIR":/out \
        phpworkbench-packager \
        makensis -DSTAGE_DIR=/stage -DOUT_FILE="/out/$EXE_NAME" /stage/installer.nsi
else
    echo "Error: makensis or Docker required to build Windows installer" >&2
    exit 1
fi

ln -sf "$EXE_NAME" "$DIST_DIR/PhpWorkbench.exe"

echo "=== Successfully built Windows Installer: $EXE_PATH ==="
ls -lh "$EXE_PATH"
