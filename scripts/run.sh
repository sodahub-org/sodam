#!/usr/bin/env bash
# 启动 GUI：给 app 自己套一层内存上限，避免它把整机拖进 swap 风暴。
#
# 用法：
#   scripts/run.sh                 # 正常启动
#   scripts/run.sh --watch         # 启动并监控 60s（CPU/内存/GPU 报错），适合排查卡顿
set -euo pipefail
cd "$(dirname "$0")/.."

MEM_MAX="${SODAM_APP_MEM_MAX:-2G}"
SWAP_MAX="${SODAM_APP_SWAP_MAX:-512M}"
BIN=target/debug/sodam

[ -x "$BIN" ] || { echo "先构建：scripts/build.sh" >&2; exit 1; }

if pgrep -x cargo >/dev/null 2>&1 || pgrep -x rustc >/dev/null 2>&1; then
    echo "错误：检测到 cargo/rustc 正在运行。等编译结束再开 GUI（见 AGENTS.md）。" >&2
    exit 1
fi

launch() {
    if command -v systemd-run >/dev/null 2>&1; then
        systemd-run --user --scope --quiet \
            -p "MemoryMax=${MEM_MAX}" -p "MemorySwapMax=${SWAP_MAX}" -- "$BIN"
    else
        "$BIN"
    fi
}

if [ "${1:-}" = "--watch" ]; then
    launch &
    APP_PID=$!
    echo "已启动（限内存 ${MEM_MAX}），监控 60 秒…"
    for _ in $(seq 1 12); do
        sleep 5
        if kill -0 "$APP_PID" 2>/dev/null; then
            ps -o pid,pcpu,rss,etime -p "$APP_PID" || true
            journalctl -k --since "-5s" --no-pager 2>/dev/null | grep -iE "drm|flip_done|gpu" || true
        else
            echo "进程已退出"; break
        fi
    done
    kill "$APP_PID" 2>/dev/null || true
    echo "监控结束，已关闭实例"
else
    launch
fi
