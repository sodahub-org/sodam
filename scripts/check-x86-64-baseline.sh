#!/usr/bin/env bash
# 发布产物指令集护栏（金丝雀）：x86_64 包里 ring 的 x25519 C 函数不许出现 AVX2 / BMI2 / AVX-512。
#
# 背景（#306）：ubuntu-26.04 的 runner 默认装 amd64v3 变体的 GCC（--with-arch-64=x86-64-v3），
# C 依赖会被编成 x86-64-v3。没有 AVX2/BMI2 的老 CPU（Ivy Bridge 等）一发起 HTTPS 就在
# TLS 握手里 SIGILL，进程内拦不住，只能在产物这一层挡。
#
# 只查编译器生成的 C 函数：ring 里由 CPUID 守着、运行时才走的 `*_adx` 汇编不查，查了是误报。
#
# 用法：scripts/check-x86-64-baseline.sh [--allow-skip] <elf>（相对路径按仓库根解析）
#   --allow-skip  非 x86-64 产物时跳过。默认是失败，免得 workflow 的 if 写漏了却一片绿。
set -euo pipefail
cd "$(dirname "$0")/.."

allow_skip=0
if [[ ${1:-} == "--allow-skip" ]]; then
    allow_skip=1
    shift
fi

binary=${1:?usage: check-x86-64-baseline.sh [--allow-skip] <elf>}
case "$binary" in
    /*) ;;
    *) binary="$PWD/$binary" ;;
esac

[[ -f "$binary" ]] || {
    echo "找不到产物：$binary" >&2
    exit 1
}

if ! header=$(readelf -h "$binary"); then
    echo "读不出 $binary 的 ELF 头（缺 binutils？），护栏失效" >&2
    exit 1
fi

if ! grep -q 'X86-64' <<<"$header"; then
    if [[ $allow_skip -eq 1 ]]; then
        echo "跳过 $binary：不是 x86-64 产物（--allow-skip）"
        exit 0
    fi
    echo "$binary 不是 x86-64 产物：这一步只给 x86_64 用，workflow 的 if 写漏了？要跳过请显式传 --allow-skip" >&2
    exit 1
fi

if ! raw_symbols=$(nm "$binary"); then
    echo "读不出 $binary 的符号表（被 strip 了，或缺 binutils？），护栏失效" >&2
    exit 1
fi

wanted=(ge_scalarmult_base public_from_private_generic_masked)
alternation=$(IFS='|'; echo "${wanted[*]}")
# 只看已定义的符号（`U`/`w`/`v` 这类外部引用和弱符号不算），后缀匹配顺带排除 `_adx` 汇编变体
symbols=$(printf '%s\n' "$raw_symbols" | awk '$2 !~ /^[UuwvW]$/ { print $NF }' |
    grep -E "x25519_(${alternation})$" || true)

# 两个都得在：少一个说明符号名变了或被 strip 了，护栏等于没装，直接报错
for want in "${wanted[@]}"; do
    if ! grep -qE "x25519_${want}$" <<<"$symbols"; then
        echo "在 $binary 里找不到 x25519_$want（被 strip 了，还是换了 ring 版本、函数改名了？），护栏失效" >&2
        exit 1
    fi
done

status=0
while IFS= read -r symbol; do
    # --no-show-raw-insn：每行固定是「地址: 助记符 操作数」，不用猜机器码那一列占多宽
    if ! disassembly=$(objdump -d --no-show-raw-insn --disassemble="$symbol" "$binary"); then
        echo "反汇编失败：$symbol" >&2
        exit 1
    fi
    # 认助记符，不去猜操作数长什么样：
    #   * baseline 的 x86-64 助记符没有 v 开头，v 前缀一律算 VEX/EVEX（verr/verw 那两条老指令除外）；
    #   * BMI1/BMI2、LZCNT、MOVBE 按助记符点名，带 b/w/l/q 后缀也算；
    #   * 操作数里出现 %ymm / %zmm / %k 掩码寄存器同样算越界。
    # 一条都没解析出来就算解析失败，不能静默放行。
    if ! hits=$(printf '%s\n' "$disassembly" | awk '
        BEGIN {
            n = split("mulx adcx adox rorx pdep pext shlx shrx sarx andn blsr blsmsk blsi bzhi bextr lzcnt tzcnt movbe", a, " ")
            for (i = 1; i <= n; i++) banned[a[i]] = 1
        }
        $1 ~ /^[0-9a-f]+:$/ {
            mnemonic = $2
            if (mnemonic !~ /^[a-z][a-z0-9]*$/) next
            seen++
            base = mnemonic
            if (base ~ /^[a-z]+[bwlq]$/) base = substr(base, 1, length(base) - 1)
            if (mnemonic ~ /^v/ && mnemonic !~ /^ver[rw]$/) {
                print
                next
            }
            if (base in banned) {
                print
                next
            }
            if ($0 ~ /%ymm|%zmm|%k[0-7]/) print
        }
        END { if (seen == 0) exit 4 }
    '); then
        echo "解析不了 objdump 的输出（$symbol），护栏失效" >&2
        exit 1
    fi
    if [[ -n "$hits" ]]; then
        echo "越界：$symbol 里有高于 x86-64 baseline 的指令（#306）" >&2
        sed -n '1,5p' <<<"$hits" >&2
        status=1
    fi
done <<<"$symbols"

if [[ $status -ne 0 ]]; then
    echo "产物要求 AVX2/BMI2：老 CPU 会在 TLS 握手时 SIGILL。检查 runner 镜像和 CFLAGS。" >&2
fi

exit $status
