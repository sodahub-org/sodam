#!/usr/bin/env bash
# 轻量检查：编译 + 测试 + lint，全部带内存护栏、限制并行度。
#
# 用法：
#   scripts/check.sh             完整检查（编译 + fmt + clippy + test）
#   scripts/check.sh --fmt-only  只检查格式，不编译（禁止本地编译时用）
#
# 背景：这台机器 15GB 内存 + zram swap，本项目依赖的 GPUI 编译/链接较重，
# 已经出现过 OOM 与整机卡死（见 AGENTS.md「资源护栏」）。所以：
#   * 一律限制 cargo 并行度（CARGO_BUILD_JOBS）
#   * 用 systemd-run 的 scope 给整条命令设内存/CPU 上限，超限只杀这条命令
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ "${1:-}" == "--fmt-only" ]]; then
    cargo fmt --all -- --check
    echo "格式化检查通过 ✅"
    exit 0
fi

if [[ $# -gt 0 ]]; then
    echo "用法：$0 [--fmt-only]" >&2
    exit 2
fi

JOBS="${SODAM_BUILD_JOBS:-4}"
MEM_MAX="${SODAM_BUILD_MEM_MAX:-6G}"
SWAP_MAX="${SODAM_BUILD_SWAP_MAX:-2G}"
CPU_QUOTA="${SODAM_BUILD_CPU_QUOTA:-400%}"

run() {
    if command -v systemd-run >/dev/null 2>&1; then
        systemd-run --user --scope --quiet \
            -p "MemoryMax=${MEM_MAX}" -p "MemorySwapMax=${SWAP_MAX}" -p "CPUQuota=${CPU_QUOTA}" \
            -- env CARGO_BUILD_JOBS="${JOBS}" "$@"
    else
        echo "提示：systemd-run 不可用，退化为 ulimit（约 6GB）" >&2
        # macOS 的 ulimit -v 不可用（Invalid argument），失败时退化为仅限并发数。
        ( ulimit -v 6291456 2>/dev/null || true; env CARGO_BUILD_JOBS="${JOBS}" "$@" )
    fi
}

echo "== cargo check（jobs=${JOBS}, mem<=${MEM_MAX}）=="
run cargo check --offline --workspace

echo "== cargo fmt --check =="
run cargo fmt --all -- --check

echo "== cargo clippy（-D warnings）=="
run cargo clippy --offline --workspace --all-targets -- -D warnings

echo "== cargo test =="
run cargo test --offline --workspace

echo "全部通过 ✅"
