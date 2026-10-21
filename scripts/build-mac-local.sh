#!/usr/bin/env bash
# Builds Concat.app on this Mac from the working tree and installs it in /Applications.
# Mirrors the macOS steps of .github/workflows/build-app.yml (sherpa static libs,
# `app` profile, dylibbundler) with an ad-hoc signature instead of notarisation.
#
#   scripts/build-mac-local.sh            build, bundle, install
#   scripts/build-mac-local.sh --no-install
#
# Needs: rustup (the toolchain comes from src/rust-toolchain.toml) and
# Homebrew's ffmpeg, cmake and dylibbundler.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"
install=1; [ "${1:-}" = "--no-install" ] && install=0

eval "$(/opt/homebrew/bin/brew shellenv zsh 2>/dev/null || /opt/homebrew/bin/brew shellenv)"
# shellcheck disable=SC1091
. "$HOME/.cargo/env"

export MACOSX_DEPLOYMENT_TARGET=12.0
# macOS 27's dyld refuses a stripped proc-macro dylib ("mis-aligned LINKEDIT string pool"),
# so rustc reports "can't find crate" for every derive. Proc macros and build scripts
# only run during the build, so they keep their symbols.
export CARGO_PROFILE_APP_BUILD_OVERRIDE_STRIP=none
export FFMPEG_DIR="$(brew --prefix ffmpeg)"
target=aarch64-apple-darwin
version=$(sed -n 's/^version = "\(.*\)"/\1/p' src/Cargo.toml | head -1)

echo ">> sherpa-onnx static libraries"
deps="$root/.deps"; mkdir -p "$deps"
sherpa_version=$(grep -A1 'name = "sherpa-onnx-sys"' src/Cargo.lock | sed -n 's/^version = "\(.*\)"/\1/p')
stem="sherpa-onnx-v$sherpa_version-osx-arm64-static-lib"
if [ ! -d "$deps/$stem/lib" ]; then
  scripts/ci/fetch.sh sherpa-onnx-osx-arm64-static-lib "$deps/sherpa.tar.bz2"
  tar -xjf "$deps/sherpa.tar.bz2" -C "$deps"
fi
export SHERPA_ONNX_LIB_DIR="$deps/$stem/lib"

echo ">> cargo build --profile app ($target, version $version)"
(cd src && cargo build --profile app -p concat --target "$target")

echo ">> bundle"
out="$root/build"; app="$out/Concat.app"
rm -rf "$app"; mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$app/Contents/Frameworks"
cp "src/target/$target/app/concat" "$app/Contents/MacOS/concat"

iconset="$out/Concat.iconset"; rm -rf "$iconset"; mkdir -p "$iconset"
icons=assets/icons
cp "$icons/concat_logo_16.png"  "$iconset/icon_16x16.png"
cp "$icons/concat_logo_32.png"  "$iconset/icon_16x16@2x.png"
cp "$icons/concat_logo_32.png"  "$iconset/icon_32x32.png"
cp "$icons/concat_logo_64.png"  "$iconset/icon_32x32@2x.png"
cp "$icons/concat_logo_128.png" "$iconset/icon_128x128.png"
cp "$icons/concat_logo_256.png" "$iconset/icon_128x128@2x.png"
cp "$icons/concat_logo_256.png" "$iconset/icon_256x256.png"
cp "$icons/concat_logo_512.png" "$iconset/icon_256x256@2x.png"
cp "$icons/concat_logo_512.png" "$iconset/icon_512x512.png"
iconutil -c icns "$iconset" -o "$app/Contents/Resources/Concat.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Concat</string>
  <key>CFBundleDisplayName</key><string>Concat</string>
  <key>CFBundleIdentifier</key><string>app.concat.editor</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleExecutable</key><string>concat</string>
  <key>CFBundleIconFile</key><string>Concat</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>${MACOSX_DEPLOYMENT_TARGET}</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.video</string>
  <key>NSHumanReadableCopyright</key><string>AGPL-3.0-or-later. See THIRD_PARTY_NOTICES.md.</string>
  <key>NSMicrophoneUsageDescription</key><string>Concat records voiceovers from your microphone.</string>
</dict>
</plist>
PLIST

dylibbundler -od -b -x "$app/Contents/MacOS/concat" -d "$app/Contents/Frameworks" -p @executable_path/../Frameworks >/dev/null
if otool -L "$app/Contents/MacOS/concat" | grep -E '/opt/homebrew|/usr/local'; then
  echo "the binary still loads libraries from outside the bundle" >&2; exit 1
fi

echo ">> ad-hoc signature"
codesign --force --deep -s - "$app"
codesign --verify --deep --strict "$app"
"$app/Contents/MacOS/concat" --version

if [ $install = 1 ]; then
  echo ">> install"
  rm -rf /Applications/Concat.app
  ditto "$app" /Applications/Concat.app
  echo "   /Applications/Concat.app ($(du -sh /Applications/Concat.app | cut -f1))"
fi
