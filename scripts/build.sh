#!/usr/bin/env bash
# 正式构建（带护栏）。**不要在 GUI 运行时执行**（见 AGENTS.md「资源护栏」）。
set -euo pipefail
cd "$(dirname "$0")/.."

JOBS="${SODAM_BUILD_JOBS:-4}"
MEM_MAX="${SODAM_BUILD_MEM_MAX:-6G}"
SWAP_MAX="${SODAM_BUILD_SWAP_MAX:-2G}"
CPU_QUOTA="${SODAM_BUILD_CPU_QUOTA:-400%}"

profile="debug"
for arg in "$@"; do
    if [[ "$arg" == "--release" ]]; then
        profile="release"
    fi
done

if pgrep -x sodam >/dev/null 2>&1; then
    echo "错误：检测到 sodam 正在运行。先关掉 GUI 再编译（避免编译峰值 + GPU 客户端叠加把机器拖死）。" >&2
    exit 1
fi

run() {
    if command -v systemd-run >/dev/null 2>&1; then
        systemd-run --user --scope --quiet \
            -p "MemoryMax=${MEM_MAX}" -p "MemorySwapMax=${SWAP_MAX}" -p "CPUQuota=${CPU_QUOTA}" \
            -- env CARGO_BUILD_JOBS="${JOBS}" "$@"
    else
        ( ulimit -v 6291456; env CARGO_BUILD_JOBS="${JOBS}" "$@" )
    fi
}

echo "== cargo build（jobs=${JOBS}, mem<=${MEM_MAX}, cpu<=${CPU_QUOTA}）=="
run cargo build --offline --workspace "$@"
echo "构建完成：target/${profile}/sodam"
