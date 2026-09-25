#!/usr/bin/env bash
# 发版后同步 Homebrew Cask 到 sodahub-org/homebrew-tap。
# 用法：packaging/homebrew/sync-tap.sh
#   1. 从 Cargo.toml 读版本号，从 Release 的 SHA256SUMS 拉 macOS 包校验和
#   2. 更新 packaging/homebrew/sodam.rb（记得把这里的改动一并提交）
#   3. 推送到 sodahub-org/homebrew-tap 的 Casks/sodam.rb
set -euo pipefail
cd "$(dirname "$0")/../.."

repo=sodahub-org/sodam
tap=sodahub-org/homebrew-tap
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
cask=packaging/homebrew/sodam.rb

[[ -n "$version" ]] || { echo "读不到 Cargo.toml 版本号" >&2; exit 1; }

sha=$(curl -fsSL "https://github.com/${repo}/releases/download/v${version}/SHA256SUMS" \
  | awk -v f="sodam-${version}-macos-aarch64.zip" '$2 == f {print $1}')
[[ ${#sha} -eq 64 ]] || { echo "SHA256SUMS 里没有 ${version} 的 macOS 包校验和" >&2; exit 1; }

sed -i '' -e 's/^  version .*/  version "'"${version}"'"/' \
          -e 's/^  sha256 .*/  sha256 "'"${sha}"'"/' "$cask"
echo "已更新 ${cask} -> ${version} (${sha})"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
git clone --depth 1 "https://github.com/${tap}.git" "$work/tap"
mkdir -p "$work/tap/Casks"
install -m644 "$cask" "$work/tap/Casks/sodam.rb"

git -C "$work/tap" add Casks/sodam.rb
if git -C "$work/tap" diff --cached --quiet; then
  echo "tap 内容无变化"
else
  git -C "$work/tap" -c user.name='ZephyrCheung' \
      -c user.email='221658147+zephyr-cheung@users.noreply.github.com' \
      commit -m "sodam ${version}"
  git -C "$work/tap" push
  echo "已推送 ${tap}"
fi
