#!/usr/bin/env bash
# Build a SodaM.app bundle from an already-built macOS binary.
# Usage: packaging/macos/make-macos-app.sh [version]
#   - expects target/release/sodam (arm64); run `cargo build --release --locked` first
#   - produces sodam-<version>-macos-aarch64.zip (+ .sha256) in the repo root
set -euo pipefail
cd "$(dirname "$0")/../.."

version=${1:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)}
[[ -x target/release/sodam ]] || {
  echo "target/release/sodam is missing; run cargo build --release first" >&2
  exit 1
}
[[ "$(uname -s)" = Darwin ]] || {
  echo "macOS .app bundling must run on macOS" >&2
  exit 1
}

app=SodaM.app
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

# ---- bundle layout ----
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
install -m755 target/release/sodam "$app/Contents/MacOS/sodam"

# ---- icon: 256px PNG -> multi-size .icns ----
iconset="$stage/SodaM.iconset"
mkdir -p "$iconset"
src=crates/sodam/assets/brand/sodam-logo.png
for size in 16 32 128 256; do
  sips -z "$size" "$size" "$src" --out "$iconset/icon_${size}x${size}.png" >/dev/null
done
for size in 16 32 128; do
  double=$((size * 2))
  sips -z "$double" "$double" "$src" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/sodam.icns"

# ---- Info.plist ----
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>zh-CN</string>
    <key>CFBundleDisplayName</key><string>SodaM</string>
    <key>CFBundleExecutable</key><string>sodam</string>
    <key>CFBundleIconFile</key><string>sodam</string>
    <key>CFBundleIdentifier</key><string>org.sodahub.sodam</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>SodaM</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>${version}</string>
    <key>CFBundleVersion</key><string>${version}</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.music</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# ---- ad-hoc signing (Apple Silicon requires at least ad-hoc signatures) ----
codesign --force --deep --sign - "$app"

# ---- zip (ditto preserves the bundle layout & signature) ----
output="sodam-${version}-macos-aarch64.zip"
rm -f "$output"
ditto -c -k --sequesterRsrc --keepParent "$app" "$output"
shasum -a 256 "$output" > "${output}.sha256"
printf '%s\n' "$output"
