#!/usr/bin/env bash
# 从已构建的 release 二进制直接打 Debian/Ubuntu `.deb` 包（不重新编译）。
#
# 用法：packaging/deb/make-deb.sh <x86_64|aarch64> [version] [release]
#
# 运行依赖：dpkg-deb（dpkg 自带）、sha256sum、install。
# 产物：sodam_<version>_<amd64|arm64>.deb 与同名 `.sha256`。
set -euo pipefail
cd "$(dirname "$0")/../.."

target_arch=${1:?usage: make-deb.sh <x86_64|aarch64> [version] [release]}
version=${2:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)}
release=${3:-1}
version=${version#v}

case "$target_arch" in
    x86_64) deb_arch=amd64 ;;
    aarch64) deb_arch=arm64 ;;
    *)
        echo "unsupported architecture: $target_arch" >&2
        exit 1
        ;;
esac

[[ -x target/release/sodam ]] || {
    echo "target/release/sodam is missing; run cargo build --release first" >&2
    exit 1
}
command -v dpkg-deb >/dev/null || {
    echo "dpkg-deb is required (Debian/Ubuntu: apt install dpkg-dev)" >&2
    exit 1
}

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

# 安装内容与 pacman 包保持一致：可执行文件、desktop 入口、hicolor 图标、许可证。
# desktop 与图标和 `packaging/make-linux-package.sh` 共用同一份源文件，
# 免得两个包安装后的菜单行为不一致。
install -Dm755 target/release/sodam "$stage/usr/bin/sodam"
install -Dm644 packaging/arch/sodam.desktop \
    "$stage/usr/share/applications/sodam.desktop"
install -Dm644 crates/sodam/assets/brand/sodam-logo.png \
    "$stage/usr/share/icons/hicolor/256x256/apps/sodam.png"
install -Dm644 LICENSE "$stage/usr/share/doc/sodam/LICENSE"
install -Dm644 NOTICE.md "$stage/usr/share/doc/sodam/NOTICE.md"

cat > "$stage/usr/share/doc/sodam/copyright" <<'COPYRIGHT'
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: sodam
Upstream-Contact: sodahub-org <https://github.com/sodahub-org/sodam/issues>
Source: https://github.com/sodahub-org/sodam

Files: *
Copyright: sodahub-org contributors
License: AGPL-3.0-or-later
 SodaM is free software: you can redistribute it and/or modify it under the
 terms of the GNU Affero General Public License as published by the Free
 Software Foundation, either version 3 of the License, or (at your option)
 any later version.
 .
 On Debian systems the full text of this license is available in
 /usr/share/common-licenses/AGPL-3, and the copy shipped with this package is
 /usr/share/doc/sodam/LICENSE.
COPYRIGHT

install -d -m 0755 "$stage/DEBIAN"

# 依赖名对照 packaging/arch/PKGBUILD：Arch 名 → Debian/Ubuntu 名。
# 用 libasound2 / libdbus-1-3 / libfontconfig1 / libfreetype6 这些传统名字，
# Ubuntu 24.04 起的 t64 包都带 `Provides:`，在 Debian 12/13 与 Ubuntu 24.04+ 都能装。
installed_size=$(du -sk "$stage" | cut -f1)
cat > "$stage/DEBIAN/control" <<CONTROL
Package: sodam
Version: ${version}-${release}
Architecture: ${deb_arch}
Maintainer: sodahub-org <sodahub-org@users.noreply.github.com>
Installed-Size: ${installed_size}
Depends: libasound2, libdbus-1-3, libfontconfig1, libfreetype6, hicolor-icon-theme, libxkbcommon0, libxcb1
Section: sound
Priority: optional
Homepage: https://github.com/sodahub-org/sodam
Description: Native Qishui Music desktop client built with GPUI
 SodaM is a third-party Qishui Music (汽水音乐) desktop client written in Rust
 and rendered with GPUI. It is not affiliated with, endorsed by, or sponsored
 by ByteDance, Douyin, or Qishui Music.
CONTROL
chmod 0644 "$stage/DEBIAN/control"

(
    cd "$stage"
    find usr -type f -print0 | sort -z | xargs -0 -r md5sum
) > "$stage/DEBIAN/md5sums"
chmod 0644 "$stage/DEBIAN/md5sums"

output="sodam_${version}_${deb_arch}.deb"
dpkg-deb --build --root-owner-group "$stage" "$output" >/dev/null

actual_arch=$(dpkg-deb --field "$output" Architecture)
[[ "$actual_arch" == "$deb_arch" ]] || {
    echo "architecture mismatch: expected $deb_arch, got $actual_arch" >&2
    exit 1
}

sha256sum "$output" > "${output}.sha256"
printf '%s\n' "$output"
