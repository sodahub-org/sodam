#!/usr/bin/env bash
# Build a pacman-compatible .pkg.tar.zst from an already-built Linux binary.
# Usage: packaging/make-linux-package.sh <x86_64|aarch64> [version] [release]
set -euo pipefail

target_arch=${1:?usage: make-linux-package.sh <x86_64|aarch64> [version] [release]}
version=${2:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)}
release=${3:-1}

case "$target_arch" in
  x86_64|aarch64) ;;
  *) echo "unsupported architecture: $target_arch" >&2; exit 1 ;;
esac
[[ -x target/release/sodam ]] || {
  echo "target/release/sodam is missing; run cargo build --release first" >&2
  exit 1
}

stage=$(mktemp -d)
trap 'find "$stage" -depth -type f -delete; find "$stage" -depth -type d -empty -delete' EXIT

install -Dm755 target/release/sodam "$stage/usr/bin/sodam"
install -Dm644 packaging/arch/sodam.desktop "$stage/usr/share/applications/sodam.desktop"
install -Dm644 crates/sodam/assets/brand/sodam-logo.png \
  "$stage/usr/share/icons/hicolor/256x256/apps/sodam.png"
install -Dm644 LICENSE "$stage/usr/share/licenses/sodam/LICENSE"

installed_size=$(du -sk "$stage" | cut -f1)
cat > "$stage/.PKGINFO" <<PKGINFO
pkgname = sodam
pkgbase = sodam
pkgver = ${version}-${release}
pkgdesc = Native Qishui Music desktop client built with GPUI
url = https://github.com/sodahub-org/sodam
builddate = $(date -u +%s)
packager = GitHub Actions <noreply@github.com>
size = ${installed_size}
arch = ${target_arch}
license = AGPL-3.0-or-later
depend = alsa-lib
depend = dbus
depend = fontconfig
depend = freetype2
depend = hicolor-icon-theme
depend = libxkbcommon
depend = libxcb
makedepend = cargo
makedepend = git
PKGINFO

output="sodam-${version}-${release}-${target_arch}.pkg.tar.zst"
tar --zstd \
  --numeric-owner --owner=0 --group=0 --mode='u+rwX,go+rX,go-w' \
  -C "$stage" \
  -cf "$output" .PKGINFO usr
sha256sum "$output" > "${output}.sha256"
printf '%s\n' "$output"
