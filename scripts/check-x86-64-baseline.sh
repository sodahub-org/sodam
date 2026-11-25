#!/usr/bin/env bash
# 发布产物指令集护栏（金丝雀）：x86_64 包里 ring 的 x25519 C 函数不许出现 AVX2 / BMI2 / AVX-512。
#
# 背景（#306）：ubuntu-26.04 的 runner 默认装 amd64v3 变体的 GCC（--with-arch-64=x86-64-v3），
# C 依赖会被编成 x86-64-v3。没有 AVX2/BMI2 的老 CPU（Ivy Bridge 等）一发起 HTTPS 就在
# TLS 握手里 SIGILL，进程内拦不住，只能在产物这一层挡。
#
# 只查编译器生成的 C 函数。ring 里由 CPUID 守着、运行时才走的 `*_adx` 汇编本来就用
# BMI2/ADX，一并查就是误报。
#
# 这是一道金丝雀：构建机的 C 基线一旦被抬高，这两个函数一定跟着抬高，不用去猜别的依赖。
# 要核对整包的话得连 glibc / crt 一起看，那是另一件事（换 runner，而不是扫指令）。
#
# 用法：scripts/check-x86-64-baseline.sh <elf>（相对路径按仓库根解析）
set -euo pipefail
cd "$(dirname "$0")/.."

binary=${1:?usage: check-x86-64-baseline.sh <elf>}
[[ -f "$binary" ]] || {
    echo "找不到产物：$binary" >&2
    exit 1
}

if ! header=$(readelf -h "$binary"); then
    echo "读不出 $binary 的 ELF 头（缺 binutils？），护栏失效" >&2
    exit 1
fi

if ! grep -q 'X86-64' <<<"$header"; then
    echo "跳过 $binary：不是 x86-64 产物"
    exit 0
fi

if ! raw_symbols=$(nm "$binary"); then
    echo "读不出 $binary 的符号表（被 strip 了，或缺 binutils？），护栏失效" >&2
    exit 1
fi

wanted=(ge_scalarmult_base public_from_private_generic_masked)
alternation=$(IFS='|'; echo "${wanted[*]}")
# 后缀匹配，顺带把 `_adx` 汇编变体排除在外
symbols=$(printf '%s\n' "$raw_symbols" | awk 'NF >= 3 { print $NF }' |
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
    if ! disassembly=$(objdump -d --disassemble="$symbol" "$binary"); then
        echo "反汇编失败：$symbol" >&2
        exit 1
    fi
    # 认助记符那一列，不去猜操作数长什么样：
    #   * baseline 的 x86-64 助记符没有 v 开头，v 前缀一律算 VEX/EVEX（含无操作数的 vzeroupper）；
    #   * BMI1/BMI2、LZCNT、MOVBE 按助记符点名，带 b/w/l/q 后缀也算；
    #   * 操作数里出现 %ymm / %zmm / %k 掩码寄存器同样算越界。
    # 一条都没解析出来（objdump 换了列分隔、换成 llvm-objdump）就算解析失败，不能静默放行。
    if ! hits=$(printf '%s\n' "$disassembly" | awk -F'\t' '
        BEGIN {
            n = split("mulx adcx adox rorx pdep pext shlx shrx sarx andn blsr blsmsk blsi bzhi bextr lzcnt tzcnt movbe", a, " ")
            for (i = 1; i <= n; i++) banned[a[i]] = 1
        }
        NF >= 3 {
            mnemonic = $3
            sub(/[ \t].*/, "", mnemonic)
            if (mnemonic !~ /^[a-z][a-z0-9]*$/) next
            seen++
            base = mnemonic
            if (base ~ /^[a-z]+[bwlq]$/) base = substr(base, 1, length(base) - 1)
            if (mnemonic ~ /^v/) {
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
        echo "$hits" | head -n 5 >&2
        status=1
    fi
done <<<"$symbols"

if [[ $status -ne 0 ]]; then
    echo "产物要求 AVX2/BMI2：老 CPU 会在 TLS 握手时 SIGILL。检查 runner 镜像和 CFLAGS。" >&2
fi

exit $status
