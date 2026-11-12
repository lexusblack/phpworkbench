# Локальная сборка PhpWorkbench (Windows, Linux, macOS)

Этот документ описывает, как собрать **кастомный движок PhpWorkbench** (`lexusblack/phpworkbench-engine`, ветка `phpworkbench`) и утилиты (`php-db`, `php-deploy`, `php-http`) локально без GitHub Actions.

---

## 1. Подготовка исходников (общая для всех ОС)

PhpWorkbench состоит из двух репозиториев: основного репозитория с инструментами/скриптами упаковки и форка движка в `vendor/zed`.

```bash
git clone https://github.com/lexusblack/phpworkbench.git
cd phpworkbench
git clone --depth 1 --branch phpworkbench https://github.com/lexusblack/phpworkbench-engine.git vendor/zed
```

Если код уже склонирован локально, обнови оба репозитория:

```bash
git pull origin main
git -C vendor/zed pull origin phpworkbench
```

Требуемая версия Rust (см. `vendor/zed/rust-toolchain.toml`): **1.98.1**.

```bash
rustup toolchain install 1.98.1
rustup default 1.98.1
```

---

## 2. Сборка на Windows (`x86_64`)

### 2.1. Зависимости

1. **Rust** через [rustup-init.exe](https://www.rust-lang.org/tools/install) (toolchain `1.98.1` + `x86_64-pc-windows-msvc`).
2. **Visual Studio 2022 / Build Tools** (workload **Desktop development with C++**) с компонентами:
   - `MSVC v143 - VS 2022 C++ x64/x86 build tools` (или `MSVC Build Tools for x64/x86 (Latest)`)
   - `C++ Spectre-mitigated libraries (Latest)` (`Microsoft.VisualStudio.Component.VC.Runtimes.x86.x64.Spectre`)
   - `Windows 11 SDK` (или `Windows 10 SDK >= 10.0.20348.0`)
   - `C++ CMake tools for Windows` (или отдельный [CMake](https://cmake.org/download/) в `PATH`)

> **Важно:** запускай команды ниже из **Developer PowerShell for VS 2022** (или убедись, что `cmake` и `cl.exe` в `PATH`).

### 2.2. Компиляция в PowerShell

```powershell
# 1. Сборка CLI-утилит
foreach ($tool in @("php-db", "php-deploy", "php-http")) {
  cargo build --release --manifest-path "tools/$tool/Cargo.toml"
}

# 2. Сборка кастомного движка PhpWorkbench
Push-Location vendor/zed
$env:ZED_RELEASE_CHANNEL = "stable"
$env:RELEASE_CHANNEL = "stable"
$env:ZED_BUNDLE = "true"
$env:CARGO_INCREMENTAL = "0"
$env:CARGO_PROFILE_RELEASE_DEBUG = "0"
$env:PHPWORKBENCH_VERSION = (Get-Content ..\..\VERSION -Raw).Trim()

cargo --config .cargo/bundle-config.toml build --release --package zed --bin zed
Pop-Location
```

### 2.3. Запуск или упаковка в `.exe`

Собранные бинарники можно скопировать в один каталог и сразу запускать `PhpWorkbench.exe` (установка не обязательна):

```powershell
New-Item -ItemType Directory -Force -Path dist/windows/bin | Out-Null
Copy-Item -Force vendor/zed/target/release/zed.exe dist/windows/bin/PhpWorkbench.exe
foreach ($tool in @("php-db", "php-deploy", "php-http")) {
  Copy-Item -Force "tools/$tool/target/release/$tool.exe" "dist/windows/bin/$tool.exe"
}
foreach ($extra in @("conpty.dll", "OpenConsole.exe")) {
  $p = "vendor/zed/target/release/$extra"
  if (Test-Path $p) { Copy-Item -Force $p "dist/windows/bin/$extra" }
}
.\dist\windows\bin\PhpWorkbench.exe
```

Если нужен инсталлятор `PhpWorkbench-Setup-<версия>.exe`, скрипт `scripts/package-exe.sh` можно запустить в **WSL (Ubuntu)** / Linux, установив `nsis` (`makensis`), `python3-pil` и `file`:

```bash
sudo apt-get update && sudo apt-get install -y nsis python3-pil file
./scripts/package-exe.sh
# Результат: dist/PhpWorkbench-Setup-1.0.1.exe
```

---

## 3. Сборка на Linux (`x86_64` / `arm64`)

### 3.1. Зависимости

Для **Ubuntu / Debian**:

```bash
sudo apt-get update && sudo apt-get install -y --no-install-recommends \
  build-essential g++ pkg-config cmake clang lld llvm curl ca-certificates git jq gettext-base \
  libasound2-dev libfontconfig-dev libgit2-dev libglib2.0-dev \
  libssl-dev libva-dev libvulkan1 libvulkan-dev libwayland-dev libx11-xcb-dev libxcb-xkb-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libzstd-dev libsqlite3-dev libssh2-1-dev \
  rpm dpkg-dev python3-pil file
```

*(На других дистрибутивах — Fedora, Arch и т.д. — список системных библиотек также можно установить скриптом `cd vendor/zed && ./script/linux`.)*

### 3.2. Компиляция и упаковка `.deb` / `.rpm`

```bash
export CC=clang
export CXX=clang++
export ZED_RELEASE_CHANNEL=stable
export RELEASE_CHANNEL=stable
export ZED_BUNDLE=true
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_RELEASE_DEBUG=0
export PHPWORKBENCH_VERSION="$(tr -d '[:space:]' < VERSION)"

ARCH="$(uname -m)" # x86_64 или aarch64
case "$ARCH" in
  x86_64)  DEB_ARCH="amd64"; RPM_ARCH="x86_64" ;;
  aarch64) DEB_ARCH="arm64"; RPM_ARCH="aarch64" ;;
  *) echo "Unsupported arch: $ARCH"; exit 1 ;;
esac

mkdir -p "dist/linux/bin/$RPM_ARCH"

# 1. Сборка CLI-утилит
for tool in php-db php-deploy php-http; do
  cargo build --release --manifest-path "tools/$tool/Cargo.toml"
  cp "tools/$tool/target/release/$tool" "dist/linux/bin/$RPM_ARCH/$tool"
  strip "dist/linux/bin/$RPM_ARCH/$tool"
done

# 2. Сборка кастомного движка PhpWorkbench
(
  cd vendor/zed
  cargo --config .cargo/bundle-config.toml build --release --package zed --bin zed
)
cp vendor/zed/target/release/zed "dist/linux/bin/$RPM_ARCH/zed"
strip "dist/linux/bin/$RPM_ARCH/zed"

# 3. Упаковка пакетов
./scripts/package-deb.sh "$DEB_ARCH"
./scripts/package-rpm.sh "$RPM_ARCH"
```

Готовые пакеты появятся в `dist/`:
- `dist/phpworkbench_${PHPWORKBENCH_VERSION}_${DEB_ARCH}.deb` → установка: `sudo apt install ./dist/phpworkbench_${PHPWORKBENCH_VERSION}_${DEB_ARCH}.deb`
- `dist/phpworkbench-${PHPWORKBENCH_VERSION}-1.${RPM_ARCH}.rpm` → установка: `sudo dnf install ./dist/phpworkbench-${PHPWORKBENCH_VERSION}-1.${RPM_ARCH}.rpm`

Либо можно запускать собранный бинарник напрямую без установки пакета: `./dist/linux/bin/$RPM_ARCH/zed`.

---

## 4. Сборка на macOS (`arm64`)

```bash
export ZED_RELEASE_CHANNEL=stable
export RELEASE_CHANNEL=stable
export ZED_BUNDLE=true
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_RELEASE_DEBUG=0
export PHPWORKBENCH_VERSION="$(tr -d '[:space:]' < VERSION)"

for tool in php-db php-deploy php-http; do
  cargo build --release --manifest-path "tools/$tool/Cargo.toml"
done

(
  cd vendor/zed
  cargo --config .cargo/bundle-config.toml build --release --package zed --bin zed
)

# Сборка PhpWorkbench.app и PhpWorkbench-<версия>.dmg
APP_DIR="dist/PhpWorkbench.app"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"
cp vendor/zed/target/release/zed "$APP_DIR/Contents/MacOS/PhpWorkbench"
strip -x "$APP_DIR/Contents/MacOS/PhpWorkbench"
for tool in php-db php-deploy php-http; do
  cp "tools/$tool/target/release/$tool" "$APP_DIR/Contents/MacOS/$tool"
  strip -x "$APP_DIR/Contents/MacOS/$tool"
done
cp design/branding/Tempest.icns "$APP_DIR/Contents/Resources/AppIcon.icns"
cp vendor/zed/crates/zed/resources/Document.icns "$APP_DIR/Contents/Resources/Document.icns"
# (плюс Contents/Info.plist из scripts/bundle-app.sh или build-packages.yml)
codesign --force --deep --sign - "$APP_DIR"
./scripts/package-dmg.sh
```
