#!/usr/bin/env bash
# 把发布产物整理成 pacman 可直接使用的仓库目录。
#
# 用法：packaging/arch/mkrepo.sh <输入目录> <输出目录>
#   * 输入目录：包含 `sodam-<ver>-<rel>-<arch>.pkg.tar.zst`
#   * 输出目录：生成 `<输出>/<arch>/{sodam.db,sodam.files,*.pkg.tar.zst}`
#
# pacman 实际请求的是 `sodam.db` / `sodam.files`，而 `repo-add` 只建同名软链；
# 静态托管（GitHub Pages / 对象存储）不保留软链，所以这里用 `cp -L` 落成实体文件。
#
# 每个架构只保留本次传入的包，历史版本不入库（旧包对 `pacman -Syu` 没有意义）。
set -euo pipefail

src=${1:?用法: mkrepo.sh <输入目录> <输出目录>}
out=${2:?用法: mkrepo.sh <输入目录> <输出目录>}

[ -d "$src" ] || { echo "输入目录不存在: $src" >&2; exit 1; }
command -v repo-add >/dev/null || { echo "缺少 repo-add（pacman 自带）" >&2; exit 1; }

mkdir -p "$out"
shopt -s nullglob

built=0
for arch in x86_64 aarch64; do
    pkgs=("$src"/sodam-*-"$arch".pkg.tar.zst)
    if [ ${#pkgs[@]} -eq 0 ]; then
        echo "跳过 $arch：没有找到 sodam-*-${arch}.pkg.tar.zst"
        continue
    fi

    dir="$out/$arch"
    rm -rf "$dir"
    mkdir -p "$dir"
    cp "${pkgs[@]}" "$dir"/

    (
        cd "$dir"
        LC_ALL=C repo-add --quiet sodam.db.tar.gz ./*.pkg.tar.zst
        # repo-add 生成的 sodam.db / sodam.files 是软链，落成实体文件
        for link in sodam.db sodam.files; do
            cp -L "$link" "$link.real"
            mv "$link.real" "$link"
        done
    )

    echo "$arch: 已入库 $(ls "$dir"/*.pkg.tar.zst | wc -l) 个包"
    built=1
done

[ "$built" -eq 1 ] || { echo "没有任何包入库" >&2; exit 1; }
echo "仓库目录已生成: $out"
