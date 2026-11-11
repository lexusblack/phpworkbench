#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DIST_DIR="$ROOT/dist"
PKG_NAME="PhpWorkbench"
VERSION="1.0.0"
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

# 2. Launcher & files
echo "2. Staging launcher scripts & resources..."
cat <<'EOF' > "$FILES_DIR/phpworkbench.cmd"
@echo off
setlocal
set "APP_DIR=%~dp0"
set "PATH=%APP_DIR%bin;%PATH%"

if exist "%APP_DIR%bin\PhpWorkbench.exe" (
    start "" "%APP_DIR%bin\PhpWorkbench.exe" %*
) else if exist "%APP_DIR%bin\zed.exe" (
    start "" "%APP_DIR%bin\zed.exe" %*
) else if exist "%APP_DIR%bin\Zed.exe" (
    start "" "%APP_DIR%bin\Zed.exe" %*
) else if exist "%LOCALAPPDATA%\Programs\Zed\zed.exe" (
    start "" "%LOCALAPPDATA%\Programs\Zed\zed.exe" %*
) else if exist "%LOCALAPPDATA%\Programs\Zed\Zed.exe" (
    start "" "%LOCALAPPDATA%\Programs\Zed\Zed.exe" %*
) else if exist "%APP_DIR%bin\Zed-Engine-Setup.exe" (
    echo Initializing PhpWorkbench core engine...
    start /wait "" "%APP_DIR%bin\Zed-Engine-Setup.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /DIR="%APP_DIR%bin"
    if exist "%APP_DIR%bin\zed.exe" (
        start "" "%APP_DIR%bin\zed.exe" %*
    ) else if exist "%APP_DIR%bin\Zed.exe" (
        start "" "%APP_DIR%bin\Zed.exe" %*
    ) else (
        echo PhpWorkbench launcher: engine extraction failed.
        pause
    )
) else (
    echo PhpWorkbench launcher: binary not found in %APP_DIR%bin
    pause
)
endlocal
EOF

# Copy Windows IDE core engine setup
if [ -f "$ROOT/dist/windows/Zed-Setup.exe" ]; then
    echo "  Staging Windows IDE core engine setup..."
    cp "$ROOT/dist/windows/Zed-Setup.exe" "$FILES_DIR/bin/Zed-Engine-Setup.exe"
fi

# Copy helper tools
for tool in php-db php-deploy php-http; do
    if [ -f "$ROOT/dist/PhpWorkbench.app/Contents/MacOS/$tool" ]; then
        cp "$ROOT/dist/PhpWorkbench.app/Contents/MacOS/$tool" "$FILES_DIR/bin/$tool.exe"
    elif [ -f "$HOME/.cargo/bin/$tool" ]; then
        cp "$HOME/.cargo/bin/$tool" "$FILES_DIR/bin/$tool.exe"
    fi
done

# Copy resources (fonts, icons, themes, keymaps, settings)
if [ -d "$ROOT/dist/PhpWorkbench.app/Contents/Resources" ]; then
    cp -R "$ROOT/dist/PhpWorkbench.app/Contents/Resources"/* "$FILES_DIR/resources/"
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

Name "$PKG_NAME"
OutFile "/out/$EXE_NAME"
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
!define MUI_ICON "/stage/phpworkbench.ico"
!define MUI_UNICON "/stage/phpworkbench.ico"

; Installer Pages
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "/stage/LICENSE.txt"
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
    File /r "/stage/files/*"

    ; Install and extract IDE core engine into bin directory
    DetailPrint "Configuring PhpWorkbench core engine..."
    IfFileExists "\$INSTDIR\\bin\\Zed-Engine-Setup.exe" 0 +3
    ExecWait '"\$INSTDIR\\bin\\Zed-Engine-Setup.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /DIR="\$INSTDIR\\bin"'
    Delete "\$INSTDIR\\bin\\Zed-Engine-Setup.exe"

    ; Create uninstaller
    WriteUninstaller "\$INSTDIR\\Uninstall.exe"

    ; Start Menu Shortcuts
    CreateDirectory "\$SMPROGRAMS\\$PKG_NAME"
    CreateShortcut "\$SMPROGRAMS\\$PKG_NAME\\$PKG_NAME.lnk" "\$INSTDIR\\phpworkbench.cmd" "" "\$INSTDIR\\resources\\phpworkbench.ico" 0
    CreateShortcut "\$SMPROGRAMS\\$PKG_NAME\\Uninstall $PKG_NAME.lnk" "\$INSTDIR\\Uninstall.exe"

    ; Desktop Shortcut
    CreateShortcut "\$DESKTOP\\$PKG_NAME.lnk" "\$INSTDIR\\phpworkbench.cmd" "" "\$INSTDIR\\resources\\phpworkbench.ico" 0

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

if docker info >/dev/null 2>&1; then
    docker run --rm \
        -v "$STAGE_DIR":/stage \
        -v "$DIST_DIR":/out \
        phpworkbench-packager \
        makensis /stage/installer.nsi
else
    echo "Error: Docker required for makensis on macOS" >&2
    exit 1
fi

ln -sf "$EXE_NAME" "$DIST_DIR/PhpWorkbench.exe"

echo "=== Successfully built Windows Installer: $EXE_PATH ==="
ls -lh "$EXE_PATH"
